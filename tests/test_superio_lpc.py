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
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import superio_lpc
from superio_lpc import (
    CONFIRMED_SENSORS,
    CONFIG_PORTS,
    EC_DATA_OFFSET,
    EC_INDEX_OFFSET,
    EC_PAGE_OFFSET,
    EC_PAGE_SELECT,
    NUVOTON_LOCK_BYTE,
    NUVOTON_UNLOCK_BYTE,
    SENSOR_STEP_DIVIDED,
    SENSOR_STEP_VOLTS,
    SuperIoReader,
    SuperIoUnavailable,
    decode_sensor_volts,
    read_board_rails,
)


class _NullMutex:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeIO:
    """Super I/O that answers as an NCT6687D and records every write."""

    CHIP_ID = 0xD592
    EC_BASE = 0x0A20

    def __init__(self, sensors=None, answer_on=0x4E):
        self.writes = []
        self.answer_on = answer_on
        self.sensors = sensors or {}
        self._config_index = None
        self._ldn = None
        self._page = None
        self._index = None
        self.driver_open = True
        self.unlocked = False

    def is_driver_open(self):
        return self.driver_open

    def outb(self, port, value):
        self.writes.append((port, value))
        if port in CONFIG_PORTS:
            if value == NUVOTON_UNLOCK_BYTE:
                self.unlocked = True
            elif value == NUVOTON_LOCK_BYTE:
                self.unlocked = False
            else:
                self._config_index = value
            return
        if port - 1 in CONFIG_PORTS:
            if self._config_index == superio_lpc.REG_LOGICAL_DEVICE:
                self._ldn = value
            return
        if port == self.EC_BASE + EC_PAGE_OFFSET:
            self._page = value
            return
        if port == self.EC_BASE + EC_INDEX_OFFSET:
            self._index = value
            return

    def inb(self, port):
        if port - 1 in CONFIG_PORTS:
            if port - 1 != self.answer_on:
                return 0xFF
            if self._config_index == superio_lpc.REG_CHIP_ID_HIGH:
                return self.CHIP_ID >> 8
            if self._config_index == superio_lpc.REG_CHIP_ID_LOW:
                return self.CHIP_ID & 0xFF
            if self._config_index == superio_lpc.REG_BASE_ADDRESS_HIGH:
                return self.EC_BASE >> 8
            if self._config_index == superio_lpc.REG_BASE_ADDRESS_LOW:
                return self.EC_BASE & 0xFF
            return 0x00
        if port == self.EC_BASE + EC_DATA_OFFSET:
            address = ((self._page or 0) << 8) | (self._index or 0)
            return self.sensors.get(address, 0x00)
        return 0x00


def make_reader(io=None):
    io = io or FakeIO()
    return SuperIoReader(io=io, mutex=_NullMutex()), io


class DetectionTest(unittest.TestCase):
    def test_detects_the_chip_and_sensor_window(self):
        reader, _ = make_reader()
        self.assertTrue(reader.detect())
        self.assertEqual(reader.chip_id, 0xD592)
        self.assertEqual(reader.chip_name, "NCT6687D")
        self.assertEqual(reader.ec_base, 0x0A20)
        self.assertEqual(reader.config_port, 0x4E)

    def test_config_mode_is_always_exited(self):
        reader, io = make_reader()
        reader.detect()
        self.assertFalse(io.unlocked, "chip was left unlocked")
        locks = [v for p, v in io.writes
                 if p in CONFIG_PORTS and v == NUVOTON_LOCK_BYTE]
        self.assertTrue(locks)

    def test_a_chip_without_this_sensor_window_is_declined(self):
        # ASUS ROG MAXIMUS Z790 APEX answers 0xD42B on config port 0x2E. It is
        # an NCT679x part, and addressing the NCT668x window on one does not
        # fault: it returns 0xFFFF, which every temperature band accepts as
        # -0.004 C. Declining the chip is the only thing that stops that.
        io = FakeIO(answer_on=0x2E)
        io.CHIP_ID = 0xD42B
        reader = SuperIoReader(io=io, mutex=_NullMutex())
        self.assertFalse(reader.detect())
        # Identity kept so the refusal can be diagnosed...
        self.assertEqual(reader.chip_id, 0xD42B)
        self.assertIn("0xD42B", reader.last_error)
        # ...but the window must not be addressable.
        self.assertIsNone(reader.ec_base)
        with self.assertRaises(SuperIoUnavailable):
            reader.read_word(0x100)

    def test_declining_a_chip_still_relocks_the_config_window(self):
        io = FakeIO(answer_on=0x2E)
        io.CHIP_ID = 0xD42B
        reader = SuperIoReader(io=io, mutex=_NullMutex())
        reader.detect()
        self.assertFalse(io.unlocked)

    def test_detection_failure_is_reported_not_raised(self):
        io = FakeIO(answer_on=None)
        io.CHIP_ID = 0xFFFF
        reader = SuperIoReader(io=io, mutex=_NullMutex())
        reader.CHIP_IDS = {}
        self.assertFalse(reader.detect())
        self.assertTrue(reader.last_error)

    def test_closed_driver_is_reported(self):
        io = FakeIO()
        io.driver_open = False
        reader, _ = make_reader(io=io)
        self.assertFalse(reader.detect())
        self.assertIn("driver", reader.last_error.lower())


class SensorReadTest(unittest.TestCase):
    def test_page_preamble_precedes_the_bank(self):
        # Without the 0xFF preamble every register returns the same byte.
        reader, io = make_reader()
        reader.detect()
        io.writes.clear()
        reader.read_word(0x126)
        page_writes = [v for p, v in io.writes
                       if p == FakeIO.EC_BASE + EC_PAGE_OFFSET]
        self.assertEqual(page_writes[0], EC_PAGE_SELECT)
        self.assertEqual(page_writes[1], 0x01)

    def test_each_byte_is_addressed_individually(self):
        # The data port does not auto-increment on this chip.
        reader, io = make_reader()
        reader.detect()
        io.writes.clear()
        reader.read_word(0x126)
        index_writes = [v for p, v in io.writes
                        if p == FakeIO.EC_BASE + EC_INDEX_OFFSET]
        self.assertEqual(index_writes, [0x26, 0x27])

    def test_word_is_big_endian(self):
        io = FakeIO(sensors={0x126: 0x2C, 0x127: 0x80})
        reader, _ = make_reader(io=io)
        reader.detect()
        self.assertEqual(reader.read_word(0x126), 0x2C80)

    def test_reading_before_detection_raises(self):
        reader, _ = make_reader()
        with self.assertRaises(SuperIoUnavailable):
            reader.read_word(0x126)


class ReadOnlyTest(unittest.TestCase):
    def test_module_has_no_sensor_write_primitive(self):
        source = open(superio_lpc.__file__, encoding="utf-8").read()
        # This chip drives fan control; a write primitive must be absent.
        for forbidden in ("def write_word", "def write_sensor", "def set_"):
            self.assertNotIn(forbidden, source)

    def test_only_selector_ports_are_written_during_a_read(self):
        reader, io = make_reader()
        reader.detect()
        io.writes.clear()
        reader.read_word(0x126)
        allowed = {
            FakeIO.EC_BASE + EC_PAGE_OFFSET,
            FakeIO.EC_BASE + EC_INDEX_OFFSET,
        }
        for port, _ in io.writes:
            self.assertIn(port, allowed,
                          "read path wrote to an unexpected port")
        self.assertNotIn(
            FakeIO.EC_BASE + EC_DATA_OFFSET, [p for p, _ in io.writes],
            "the data port must never be written",
        )


class ConfirmedRailTest(unittest.TestCase):
    def test_vddio_scale_matches_the_bios_calibration(self):
        # CPU VDDIO 1.47 -> 1.41 V moved 0x0126 by -480 counts, giving exactly
        # 0.125 mV per count with no divider.
        address, step = CONFIRMED_SENSORS["vddio_mem"]
        self.assertEqual(address, 0x126)
        self.assertAlmostEqual(step, 0.000125)
        self.assertAlmostEqual(SENSOR_STEP_VOLTS, 0.000125)

    def test_vddio_readings_decode_to_the_bios_values(self):
        self.assertAlmostEqual(decode_sensor_volts(0x2E60), 1.484, places=4)
        self.assertAlmostEqual(decode_sensor_volts(0x2C80), 1.424, places=4)
        # The delta must equal the BIOS change exactly.
        self.assertAlmostEqual(
            decode_sensor_volts(0x2C80) - decode_sensor_volts(0x2E60),
            -0.060, places=6,
        )

    def test_vtt_matches_both_hwinfo_endpoints(self):
        # HWiNFO over a TM5 run: VTT min 2.044, max 2.046. The channel ranged
        # 4.0880-4.0920 raw, which halves to exactly those two values.
        address, step = CONFIRMED_SENSORS["vtt"]
        self.assertEqual(address, 0x136)
        self.assertAlmostEqual(step, SENSOR_STEP_DIVIDED)
        self.assertAlmostEqual(
            decode_sensor_volts(0x7FC0, step), 2.0440, places=4
        )
        self.assertAlmostEqual(
            decode_sensor_volts(0x7FE0, step), 2.0460, places=4
        )

    def test_core_rail_is_not_served_from_this_chip(self):
        # 0x0128 is the VRM-side Vcore measurement and does track load
        # (2.5520 -> 2.3240 raw, i.e. 1.2760 -> 1.1620 V halved), but
        # VDDCR_VDD is served from the CPU's own SVI3 telemetry in the PM
        # table so it matches HWiNFO's "(SVI3 TFN)" reading instead.
        self.assertNotIn("vddcr_vdd", CONFIRMED_SENSORS)
        self.assertAlmostEqual(
            decode_sensor_volts(0x4FC0, SENSOR_STEP_DIVIDED), 1.2760, places=3
        )

    def test_divider_reproduces_the_cpu_nb_soc_cross_check(self):
        # 0x0124 halves to CPU NB/SoC 1.206 V exactly. That second fixed rail
        # is what establishes the divider rather than fitting it to Vcore.
        self.assertAlmostEqual(
            decode_sensor_volts(0x4B60, SENSOR_STEP_DIVIDED), 1.2060, places=4
        )

    def test_read_board_rails_returns_the_decoded_rails(self):
        io = FakeIO(sensors={
            0x126: 0x2C, 0x127: 0x80,     # VDDIO 1.424
            0x136: 0x7F, 0x137: 0xE0,     # VTT   2.046
        })
        rails = read_board_rails(
            reader_factory=lambda: SuperIoReader(io=io, mutex=_NullMutex())
        )
        self.assertAlmostEqual(rails["vddio_mem"], 1.424, places=4)
        self.assertAlmostEqual(rails["vtt"], 2.046, places=4)

    def test_out_of_range_reading_is_dropped(self):
        io = FakeIO(sensors={0x126: 0xFF, 0x127: 0xFF})
        rails = read_board_rails(
            reader_factory=lambda: SuperIoReader(io=io, mutex=_NullMutex())
        )
        self.assertNotIn("vddio_mem", rails)

    def test_detection_failure_yields_no_rails(self):
        io = FakeIO(answer_on=None)
        rails = read_board_rails(
            reader_factory=lambda: SuperIoReader(io=io, mutex=_NullMutex())
        )
        self.assertEqual(rails, {})

    def test_only_the_board_sensor_rails_are_claimed(self):
        self.assertEqual(set(CONFIRMED_SENSORS), {"vddio_mem", "vtt"})


if __name__ == "__main__":
    unittest.main()
