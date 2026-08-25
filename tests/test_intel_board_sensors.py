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

"""Cover the Intel board-sensor rails and the evidence gate in front of them."""

import unittest

import intel_board_sensors
from intel_board_sensors import (
    CONFIRMED_RAILS,
    CONFIRMED_TEMPERATURES,
    INTEL_RAILS,
    INTEL_TEMPERATURES,
    STEP_REFERENCE_CAPTURE,
    decode_temperature,
    read_board_rails,
    read_board_temperatures,
    rail_text,
    temperature_text,
    validate_rail,
    validate_temperature,
)

# The 0x100 block as swept on the Z790 target, against HWiNFO reading the same
# chip: CPU, System, MOS, PCH, CPU Socket, T0, T1.
TEMPERATURE_CAPTURE = {
    0x100: 0x3600,   # 54.0
    0x102: 0x2300,   # 35.0
    0x104: 0x2380,   # 35.5
    0x106: 0x2800,   # 40.0
    0x108: 0x2200,   # 34.0
    0x10A: 0x0B00,   # 11.0  - the T0 anchor
    0x10C: 0x1F00,   # 31.0
}


class _FakeReader:
    """Stands in for SuperIoReader. Never touches the ISA path."""

    def __init__(self, words=None, detects=True):
        self.words = words or {}
        self.detects = detects
        self.reads = []

    def detect(self):
        return self.detects

    def read_word(self, address):
        self.reads.append(address)
        if address not in self.words:
            raise OSError("no sensor at 0x%04X" % address)
        return self.words[address]


class ConfirmedMappingTest(unittest.TestCase):
    """The mapping must keep decoding to the values it was confirmed against."""

    def test_every_confirmed_rail_is_a_known_rail(self):
        for key in CONFIRMED_RAILS:
            with self.subTest(key=key):
                self.assertIn(key, INTEL_RAILS)

    def test_dram_decodes_to_the_value_hwinfo_reported(self):
        address, step = CONFIRMED_RAILS["vdimm"]
        volts = STEP_REFERENCE_CAPTURE[address] * step
        self.assertAlmostEqual(volts, 1.572, places=3)

    def test_cpu_aux_decodes_to_the_value_hwinfo_reported(self):
        address, step = CONFIRMED_RAILS["cpu_aux"]
        volts = STEP_REFERENCE_CAPTURE[address] * step
        self.assertAlmostEqual(volts, 1.816, places=3)

    def test_every_confirmed_rail_decodes_inside_its_band(self):
        for key, (address, step) in CONFIRMED_RAILS.items():
            with self.subTest(key=key):
                volts = STEP_REFERENCE_CAPTURE[address] * step
                self.assertIsNotNone(validate_rail(key, volts))

    def test_an_empty_map_reports_nothing_and_opens_no_path(self):
        reader = _FakeReader({0x128: 0x3120})
        self.assertEqual(
            read_board_rails(reader_factory=lambda: reader, sensors={}), {}
        )
        self.assertEqual(reader.reads, [])


class DetectionRefusalTest(unittest.TestCase):
    """A board whose chip declines must report nothing, and only ask once."""

    def setUp(self):
        intel_board_sensors._DETECTED.clear()
        self.addCleanup(intel_board_sensors._DETECTED.clear)

    def test_a_declining_chip_reports_no_temperatures(self):
        reader = _FakeReader(TEMPERATURE_CAPTURE, detects=False)
        self.assertEqual(
            read_board_temperatures(reader_factory=lambda: reader), {}
        )
        self.assertEqual(reader.reads, [])

    def test_a_declining_chip_reports_no_rails(self):
        reader = _FakeReader({0x128: 0x3120}, detects=False)
        self.assertEqual(read_board_rails(reader_factory=lambda: reader), {})
        self.assertEqual(reader.reads, [])

    def test_the_refusal_is_cached_so_the_isa_path_is_not_reprobed(self):
        # Summary re-reads every tick. Without a cached refusal each tick takes
        # the monitoring mutex to re-ask a chip that cannot change.
        reader = _FakeReader(TEMPERATURE_CAPTURE, detects=False)
        attempts = []

        def factory():
            attempts.append(1)
            return reader

        for _ in range(5):
            read_board_temperatures(reader_factory=factory)
        self.assertEqual(len(attempts), 1)


class RailValidationTest(unittest.TestCase):
    def test_reading_inside_the_band_is_kept(self):
        self.assertEqual(validate_rail("vdimm", 1.568), 1.568)

    def test_reading_outside_the_band_is_dropped(self):
        self.assertIsNone(validate_rail("vdimm", 16.032))
        self.assertIsNone(validate_rail("vdimm", 0.0))

    def test_band_edges_are_inclusive(self):
        _label, minimum, maximum = INTEL_RAILS["vdimm"]
        self.assertEqual(validate_rail("vdimm", minimum), minimum)
        self.assertEqual(validate_rail("vdimm", maximum), maximum)

    def test_unknown_rail_is_dropped(self):
        self.assertIsNone(validate_rail("nonexistent", 1.2))

    def test_unreadable_value_is_dropped(self):
        self.assertIsNone(validate_rail("vdimm", None))
        self.assertIsNone(validate_rail("vdimm", "not a voltage"))


class ConfirmedRailReadTest(unittest.TestCase):
    """Behaviour once a rail is confirmed, driven through the sensors argument."""

    def setUp(self):
        intel_board_sensors._DETECTED.clear()
        self.addCleanup(intel_board_sensors._DETECTED.clear)

    def test_a_confirmed_rail_is_decoded_and_returned(self):
        # 0x3100 counts at 0.125 mV each is 1.568 V.
        reader = _FakeReader({0x126: 0x3100})
        rails = read_board_rails(
            reader_factory=lambda: reader,
            sensors={"vdimm": (0x126, 0.000125)},
        )
        self.assertAlmostEqual(rails["vdimm"], 1.568, places=3)
        self.assertEqual(reader.reads, [0x126])

    def test_a_decode_outside_the_band_is_left_out(self):
        # 0xD080 at the full step is 6.672 V, which no DRAM rail can be.
        reader = _FakeReader({0x130: 0xD080})
        rails = read_board_rails(
            reader_factory=lambda: reader,
            sensors={"vdimm": (0x130, 0.000125)},
        )
        self.assertEqual(rails, {})

    def test_a_sensor_that_does_not_answer_is_left_out(self):
        reader = _FakeReader({})
        rails = read_board_rails(
            reader_factory=lambda: reader,
            sensors={"vdimm": (0x126, 0.000125)},
        )
        self.assertEqual(rails, {})

    def test_failed_detection_yields_no_rails(self):
        reader = _FakeReader({0x126: 0x3100}, detects=False)
        rails = read_board_rails(
            reader_factory=lambda: reader,
            sensors={"vdimm": (0x126, 0.000125)},
        )
        self.assertEqual(rails, {})
        self.assertEqual(reader.reads, [])

    def test_detection_is_reused_across_reads(self):
        reader = _FakeReader({0x126: 0x3100})
        factory = lambda: reader
        sensors = {"vdimm": (0x126, 0.000125)}
        read_board_rails(reader_factory=factory, sensors=sensors)
        read_board_rails(reader_factory=factory, sensors=sensors)
        self.assertEqual(reader.reads, [0x126, 0x126])


class MovingRailTest(unittest.TestCase):
    """Vcore and CPU SA, the two rails a single capture could not pin."""

    def test_vcore_decodes_to_the_idle_value_that_was_measured(self):
        address, step = CONFIRMED_RAILS["vcore"]
        self.assertAlmostEqual(
            STEP_REFERENCE_CAPTURE[address] * step, 1.306, places=3
        )

    def test_cpu_sa_decodes_to_the_value_two_instruments_agreed_on(self):
        address, step = CONFIRMED_RAILS["cpu_sa"]
        self.assertAlmostEqual(
            STEP_REFERENCE_CAPTURE[address] * step, 1.292, places=3
        )

    def test_both_moving_rails_use_the_halved_step(self):
        full_step = CONFIRMED_RAILS["vdimm"][1]
        for key in ("vcore", "cpu_sa"):
            with self.subTest(key=key):
                self.assertAlmostEqual(CONFIRMED_RAILS[key][1], full_step / 2)

    def test_the_droop_measured_under_load_stays_inside_the_band(self):
        # 0x0124 fell to 2.5240 raw under an all-core load.
        _address, step = CONFIRMED_RAILS["vcore"]
        self.assertIsNotNone(validate_rail("vcore", 0x51A0 * step))
        self.assertIsNotNone(validate_rail("vcore", 20192 * step))


class TemperatureDecodeTest(unittest.TestCase):
    def test_the_high_byte_is_whole_degrees(self):
        self.assertEqual(decode_temperature(0x2800), 40.0)

    def test_the_low_byte_is_a_fraction_of_a_degree(self):
        self.assertEqual(decode_temperature(0x2380), 35.5)

    def test_a_negative_reading_decodes(self):
        self.assertEqual(decode_temperature(0xFB00), -5.0)

    def test_zero_decodes(self):
        self.assertEqual(decode_temperature(0x0000), 0.0)

    def test_every_captured_sensor_decodes_to_what_hwinfo_listed(self):
        expected = {
            0x100: 54.0, 0x102: 35.0, 0x104: 35.5, 0x106: 40.0,
            0x108: 34.0, 0x10A: 11.0, 0x10C: 31.0,
        }
        for address, raw in TEMPERATURE_CAPTURE.items():
            with self.subTest(address=address):
                self.assertEqual(decode_temperature(raw), expected[address])


class TemperatureValidationTest(unittest.TestCase):
    def test_a_reading_inside_the_band_is_kept(self):
        self.assertEqual(validate_temperature("vrm", 35.5), 35.5)

    def test_a_reading_outside_the_band_is_dropped(self):
        self.assertIsNone(validate_temperature("cpu", 300.0))
        self.assertIsNone(validate_temperature("cpu", -100.0))

    def test_an_unknown_sensor_is_dropped(self):
        self.assertIsNone(validate_temperature("nonexistent", 40.0))

    def test_an_unreadable_value_is_dropped(self):
        self.assertIsNone(validate_temperature("cpu", None))
        self.assertIsNone(validate_temperature("cpu", "not a temperature"))

    def test_the_ceiling_sits_under_what_the_high_byte_can_hold(self):
        # A signed high byte tops out at 127, so a bound above that could
        # never reject anything.
        for key, (_label, _minimum, maximum) in INTEL_TEMPERATURES.items():
            with self.subTest(key=key):
                self.assertLess(maximum, 127.0)


class BoardTemperatureReadTest(unittest.TestCase):
    def setUp(self):
        intel_board_sensors._DETECTED.clear()
        self.addCleanup(intel_board_sensors._DETECTED.clear)

    def test_every_confirmed_sensor_is_a_known_sensor(self):
        for key in CONFIRMED_TEMPERATURES:
            with self.subTest(key=key):
                self.assertIn(key, INTEL_TEMPERATURES)

    def test_the_thermistor_headers_are_not_displayed(self):
        # T0 reads 11 C with nothing attached, so neither header is a row.
        self.assertNotIn(0x10A, CONFIRMED_TEMPERATURES.values())
        self.assertNotIn(0x10C, CONFIRMED_TEMPERATURES.values())

    def test_the_captured_block_reads_back_as_the_board_sensors(self):
        reader = _FakeReader(TEMPERATURE_CAPTURE)
        values = read_board_temperatures(reader_factory=lambda: reader)
        self.assertEqual(values["cpu"], 54.0)
        self.assertEqual(values["system"], 35.0)
        self.assertEqual(values["vrm"], 35.5)
        self.assertEqual(values["pch"], 40.0)
        self.assertEqual(values["socket"], 34.0)

    def test_a_sensor_that_does_not_answer_is_left_out(self):
        reader = _FakeReader({0x104: 0x2380})
        values = read_board_temperatures(reader_factory=lambda: reader)
        self.assertEqual(values, {"vrm": 35.5})

    def test_an_implausible_reading_is_left_out(self):
        reader = _FakeReader({0x104: 0x7F00})
        self.assertEqual(
            read_board_temperatures(reader_factory=lambda: reader), {}
        )

    def test_failed_detection_yields_no_sensors(self):
        reader = _FakeReader(TEMPERATURE_CAPTURE, detects=False)
        self.assertEqual(
            read_board_temperatures(reader_factory=lambda: reader), {}
        )
        self.assertEqual(reader.reads, [])

    def test_an_empty_map_reads_nothing(self):
        reader = _FakeReader(TEMPERATURE_CAPTURE)
        self.assertEqual(
            read_board_temperatures(reader_factory=lambda: reader, sensors={}),
            {},
        )
        self.assertEqual(reader.reads, [])

    def test_formatting_carries_one_decimal_and_a_unit(self):
        reader = _FakeReader(TEMPERATURE_CAPTURE)
        self.assertEqual(
            temperature_text("vrm", reader_factory=lambda: reader), "35.5 °C"
        )

    def test_formatting_an_absent_sensor_reports_nothing(self):
        reader = _FakeReader({})
        self.assertIsNone(
            temperature_text("vrm", reader_factory=lambda: reader)
        )


if __name__ == "__main__":
    unittest.main()
