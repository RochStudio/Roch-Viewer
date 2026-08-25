"""Cover the NCT679x transport and the evidence behind its sensor map."""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import nct679x
from nct679x import (
    BANK_SELECT_REGISTER,
    HWM_DATA_OFFSET,
    HWM_INDEX_OFFSET,
    Nct679xReader,
    Nct679xUnavailable,
    decode_temperature,
    decode_volts,
)
from superio_lpc import (
    CONFIG_PORTS,
    NUVOTON_LOCK_BYTE,
    NUVOTON_UNLOCK_BYTE,
    REG_BASE_ADDRESS_HIGH,
    REG_BASE_ADDRESS_LOW,
    REG_CHIP_ID_HIGH,
    REG_CHIP_ID_LOW,
    REG_LOGICAL_DEVICE,
)


class _NullMutex:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeIO:
    """Answers as the NCT6798D on an ASUS ROG MAXIMUS Z790 APEX."""

    CHIP_ID = 0xD42B
    HWM_BASE = 0x0290

    def __init__(self, sensors=None, answer_on=0x2E):
        self.writes = []
        self.answer_on = answer_on
        self.sensors = sensors or {}
        self._config_index = None
        self._bank = 0
        self._index = 0
        self._selecting_bank = False
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
            return
        if port == self.HWM_BASE + HWM_INDEX_OFFSET:
            if value == BANK_SELECT_REGISTER:
                self._selecting_bank = True
            else:
                self._index = value
                self._selecting_bank = False
            return
        if port == self.HWM_BASE + HWM_DATA_OFFSET and self._selecting_bank:
            self._bank = value
            self._selecting_bank = False
            return

    def inb(self, port):
        if port - 1 in CONFIG_PORTS:
            if port - 1 != self.answer_on:
                return 0xFF
            if self._config_index == REG_CHIP_ID_HIGH:
                return self.CHIP_ID >> 8
            if self._config_index == REG_CHIP_ID_LOW:
                return self.CHIP_ID & 0xFF
            if self._config_index == REG_BASE_ADDRESS_HIGH:
                return self.HWM_BASE >> 8
            if self._config_index == REG_BASE_ADDRESS_LOW:
                return self.HWM_BASE & 0xFF
            return 0x00
        if port == self.HWM_BASE + HWM_DATA_OFFSET:
            return self.sensors.get((self._bank << 8) | self._index, 0x00)
        return 0x00


def make_reader(io=None):
    io = io or FakeIO()
    return Nct679xReader(io=io, mutex=_NullMutex()), io


class DetectionTest(unittest.TestCase):
    def test_detects_the_chip_and_its_monitor_window(self):
        reader, _ = make_reader()
        self.assertTrue(reader.detect())
        self.assertEqual(reader.chip_id, 0xD42B)
        self.assertEqual(reader.chip_name, "NCT6798D")
        self.assertEqual(reader.hwm_base, 0x0290)
        self.assertEqual(reader.config_port, 0x2E)

    def test_an_unknown_chip_is_declined_and_leaves_no_window(self):
        io = FakeIO()
        io.CHIP_ID = 0xD592           # NCT6687D: a real chip, but not this family
        reader = Nct679xReader(io=io, mutex=_NullMutex())
        self.assertFalse(reader.detect())
        self.assertIsNone(reader.hwm_base)
        self.assertIn("0xD592", reader.last_error)

    def test_detection_relocks_the_config_window(self):
        reader, io = make_reader()
        reader.detect()
        self.assertFalse(io.unlocked)

    def test_declining_also_relocks_the_config_window(self):
        io = FakeIO()
        io.CHIP_ID = 0xD592
        Nct679xReader(io=io, mutex=_NullMutex()).detect()
        self.assertFalse(io.unlocked)

    def test_reading_before_detection_is_refused(self):
        reader, _ = make_reader()
        with self.assertRaises(Nct679xUnavailable):
            reader.read_byte(0x491)

    def test_a_closed_driver_is_reported_not_raised(self):
        io = FakeIO()
        io.driver_open = False
        reader = Nct679xReader(io=io, mutex=_NullMutex())
        self.assertFalse(reader.detect())
        self.assertIn("driver", reader.last_error)


class RegisterAddressingTest(unittest.TestCase):
    def test_a_sensor_byte_is_addressed_bank_then_register(self):
        reader, io = make_reader(FakeIO({0x491: 0x21}))
        self.assertTrue(reader.detect())
        io.writes.clear()
        self.assertEqual(reader.read_byte(0x491), 0x21)
        # Bank select on the index port, bank on the data port, then register.
        self.assertEqual(io.writes, [
            (0x0290 + HWM_INDEX_OFFSET, BANK_SELECT_REGISTER),
            (0x0290 + HWM_DATA_OFFSET, 0x04),
            (0x0290 + HWM_INDEX_OFFSET, 0x91),
        ])

    def test_bank_zero_registers_read_from_bank_zero(self):
        reader, _ = make_reader(FakeIO({0x027: 0x1F}))
        reader.detect()
        self.assertEqual(reader.read_byte(0x027), 0x1F)

    def test_several_bytes_read_under_one_lock(self):
        reader, _ = make_reader(FakeIO({0x491: 0x21, 0x401: 0x31}))
        reader.detect()
        self.assertEqual(reader.read_bytes([0x491, 0x401]), [0x21, 0x31])


class DecodeTest(unittest.TestCase):
    def test_whole_degrees(self):
        self.assertEqual(decode_temperature(0x21), 33)
        self.assertEqual(decode_temperature(0x31), 49)
        self.assertEqual(decode_temperature(0x1F), 31)

    def test_a_negative_reading_is_signed(self):
        self.assertEqual(decode_temperature(0xFF), -1)
        self.assertEqual(decode_temperature(0x80), -128)

    def test_volts_at_the_per_channel_step(self):
        self.assertAlmostEqual(decode_volts(0x92, 0.009), 1.314, places=3)
        self.assertAlmostEqual(decode_volts(0x4A, 0.016), 1.184, places=3)
        self.assertAlmostEqual(decode_volts(0x73, 0.016), 1.840, places=3)
        self.assertAlmostEqual(decode_volts(0x4C, 0.018), 1.368, places=3)

    def test_the_block_is_not_uniformly_eight_millivolts(self):
        # The trap this map fell into once: the droop identified the core
        # channel, and the block's usual step was assumed to come with it.
        # At 8 mV the same raw reads 1.168 V against HWiNFO's 1.314.
        self.assertAlmostEqual(decode_volts(0x92, 0.008), 1.168, places=3)
        from intel_board_sensors import NCT6798D_RAILS

        steps = {step for _address, step in NCT6798D_RAILS.values()}
        self.assertGreater(len(steps), 1)

    def test_the_memory_controller_step_is_two_counts_across_hwinfo_range(self):
        # HWiNFO watched IMC VDD move 1.332-1.368 V over the capture window.
        # At 18 mV that is raw 74 to 76; no other step puts both endpoints on
        # whole counts two apart, which is what fixes the step independently
        # of the single-point match.
        self.assertAlmostEqual(decode_volts(74, 0.018), 1.332, places=3)
        self.assertAlmostEqual(decode_volts(76, 0.018), 1.368, places=3)


class ConfirmedCaptureTest(unittest.TestCase):
    """The map must keep decoding to what HWiNFO read on the same boot.

    Every value below came off the ASUS ROG MAXIMUS Z790 APEX bench with
    HWiNFO reading the same NCT6798D, and each was then put through an
    idle -> all-core load -> idle cycle before being claimed.
    """

    CAPTURE = {
        0x491: 0x21,   # CPU        33 C, rises to 41-42 C loaded
        0x401: 0x31,   # PCH        49 C, flat under load
        0x027: 0x1F,   # System     31 C, flat under load
        0x480: 0x92,   # Vcore      1.314 V at 9 mV, droops under load
        0x489: 0x41,   # VTT        1.040 V (HWiNFO's minimum for the channel)
        0x48A: 0x4C,   # VDD2       1.368 V (HWiNFO "IMC VDD")
        0x48D: 0x4A,   # CPU SA     1.184 V
        0x48E: 0x73,   # CPU AUX    1.840 V
        0x0FB: 0x24,   # not mapped: reads 36 but never moves. See below.
    }

    def test_temperatures_decode_to_the_hwinfo_readings(self):
        from intel_board_sensors import NCT6798D_TEMPERATURES

        expected = {"cpu": 33, "pch": 49, "system": 31}
        for key, address in NCT6798D_TEMPERATURES.items():
            with self.subTest(key=key):
                self.assertEqual(
                    decode_temperature(self.CAPTURE[address]), expected[key]
                )

    def test_rails_decode_to_the_hwinfo_readings(self):
        from intel_board_sensors import NCT6798D_RAILS

        expected = {
            "vcore": 1.314, "vtt": 1.040, "vdd2": 1.368,
            "cpu_sa": 1.184, "cpu_aux": 1.840,
        }
        for key, (address, step) in NCT6798D_RAILS.items():
            with self.subTest(key=key):
                self.assertAlmostEqual(
                    decode_volts(self.CAPTURE[address], step),
                    expected[key], places=3,
                )

    def test_every_mapped_reading_lands_inside_its_band(self):
        from intel_board_sensors import (
            NCT6798D_RAILS, NCT6798D_TEMPERATURES, validate_rail,
            validate_temperature,
        )

        for key, address in NCT6798D_TEMPERATURES.items():
            with self.subTest(temperature=key):
                self.assertIsNotNone(validate_temperature(
                    key, decode_temperature(self.CAPTURE[address])))
        for key, (address, step) in NCT6798D_RAILS.items():
            with self.subTest(rail=key):
                self.assertIsNotNone(validate_rail(
                    key, decode_volts(self.CAPTURE[address], step)))

    def test_the_static_lookalike_is_not_mapped(self):
        # 0x0FB reads exactly 36, the sole match anywhere in the register space
        # for HWiNFO's "CPU Package 36 C", and it is still not a CPU sensor: it
        # did not move a degree across a full load cycle. Claiming it would be
        # the resemblance error this project keeps refusing to make.
        from intel_board_sensors import NCT6798D_TEMPERATURES

        self.assertNotIn(0x0FB, NCT6798D_TEMPERATURES.values())

    def test_sensors_this_board_does_not_carry_stay_unmapped(self):
        # HWiNFO lists no VRM or socket sensor under this chip; its VR VCC
        # temperature is SVID telemetry on another transport. A blank row beats
        # a borrowed one.
        from intel_board_sensors import NCT6798D_RAILS, NCT6798D_TEMPERATURES

        self.assertNotIn("vrm", NCT6798D_TEMPERATURES)
        self.assertNotIn("socket", NCT6798D_TEMPERATURES)
        # DRAM comes from the DDR5 PMIC on this board, not the Super I/O.
        self.assertNotIn("vdimm", NCT6798D_RAILS)


class SafetyTest(unittest.TestCase):
    def test_module_defines_no_write_primitive(self):
        source = open(nct679x.__file__, encoding="utf-8").read()
        # Selection writes go through _read_locked and the config helpers only.
        self.assertNotIn("def write", source)


if __name__ == "__main__":
    unittest.main()
