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

"""Cover the ACPI EC read protocol, its read-only contract and the VRM map."""

import unittest

import asus_ec
from asus_ec import (
    COMMAND_READ,
    CONFIRMED_EC_TEMPERATURES,
    EC_COMMAND_PORT,
    EC_DATA_PORT,
    STATUS_INPUT_FULL,
    STATUS_OUTPUT_FULL,
    AcpiEcReader,
    EcUnavailable,
    decode_temperature,
    is_asus_board,
    read_temperatures,
    validate_temperature,
)


class NullMutex:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class FakeEc:
    """An embedded controller that answers the standard read handshake."""

    def __init__(self, ram=None, driver_open=True, stuck=False,
                 output_full_at_start=False):
        self.ram = dict(ram or {})
        self.writes = []
        self.driver_open = driver_open
        self.stuck = stuck
        self._status = STATUS_OUTPUT_FULL if output_full_at_start else 0x00
        self._expect_address = False
        self._pending = None

    def is_driver_open(self):
        return self.driver_open

    def inb(self, port):
        if port == EC_COMMAND_PORT:
            return 0xFF if self.stuck else self._status
        if port == EC_DATA_PORT:
            self._status &= ~STATUS_OUTPUT_FULL
            value, self._pending = self._pending, None
            return 0x00 if value is None else value
        return 0x00

    def outb(self, port, value):
        self.writes.append((port, value))
        if port == EC_COMMAND_PORT and value == COMMAND_READ:
            self._expect_address = True
            return
        if port == EC_DATA_PORT and self._expect_address:
            self._expect_address = False
            self._pending = self.ram.get(value, 0x00)
            self._status |= STATUS_OUTPUT_FULL


def make_reader(io):
    return AcpiEcReader(io=io, mutex=NullMutex(), sleep=lambda _s: None)


class ProtocolTest(unittest.TestCase):
    def test_a_register_is_read_through_the_standard_handshake(self):
        io = FakeEc({0x33: 40})
        self.assertEqual(make_reader(io).read_byte(0x33), 40)
        self.assertEqual(io.writes, [
            (EC_COMMAND_PORT, COMMAND_READ),
            (EC_DATA_PORT, 0x33),
        ])

    def test_a_byte_left_by_another_owner_is_discarded(self):
        # Starting with OBF set would otherwise return someone else's byte as
        # though it were the answer to our address.
        io = FakeEc({0x33: 40}, output_full_at_start=True)
        self.assertEqual(make_reader(io).read_byte(0x33), 40)

    def test_a_controller_that_stops_answering_times_out(self):
        io = FakeEc({0x33: 40}, stuck=True)
        with self.assertRaises(TimeoutError):
            make_reader(io).read_byte(0x33)

    def test_no_driver_is_reported_not_raised_as_something_else(self):
        io = FakeEc({0x33: 40}, driver_open=False)
        with self.assertRaises(EcUnavailable):
            make_reader(io).read_byte(0x33)

    def test_several_registers_read_under_one_lock(self):
        io = FakeEc({0x33: 40, 0x30: 33})
        self.assertEqual(make_reader(io).read_bytes([0x33, 0x30]), [40, 33])

    def test_an_unreasonable_timeout_is_refused(self):
        with self.assertRaises(ValueError):
            AcpiEcReader(io=FakeEc(), mutex=NullMutex(), timeout=5.0)


class ReadOnlyContractTest(unittest.TestCase):
    """The EC owns fan control on many boards; this module must not write."""

    def test_no_write_command_is_defined(self):
        # Checked against the module's namespace, not its text: the docstring
        # names WR_EC precisely to explain why it is absent, and a test that
        # forbade the word would forbid saying so.
        for name, value in vars(asus_ec).items():
            if name.startswith("__"):
                continue
            with self.subTest(name=name):
                if isinstance(value, int) and not isinstance(value, bool):
                    self.assertNotEqual(
                        value, 0x81, "WR_EC must not be defined"
                    )
                self.assertNotIn("write", name.lower())

    def test_no_method_can_write_a_register(self):
        for name in dir(AcpiEcReader):
            with self.subTest(name=name):
                self.assertNotIn("write", name.lower())

    def test_only_the_command_and_data_ports_are_written(self):
        io = FakeEc({0x33: 40})
        make_reader(io).read_byte(0x33)
        self.assertEqual(
            {port for port, _value in io.writes},
            {EC_COMMAND_PORT, EC_DATA_PORT},
        )

    def test_the_only_command_byte_written_is_the_read_command(self):
        io = FakeEc({0x33: 40})
        make_reader(io).read_byte(0x33)
        commands = [v for port, v in io.writes if port == EC_COMMAND_PORT]
        self.assertEqual(commands, [COMMAND_READ])


class BoardGateTest(unittest.TestCase):
    def test_an_asus_board_is_recognised(self):
        self.assertTrue(is_asus_board(
            "ASUSTeK COMPUTER INC. ROG MAXIMUS Z790 APEX (Rev 1.xx)"))
        self.assertTrue(is_asus_board("asus rog strix"))

    def test_another_vendor_is_not(self):
        # The register map is ASUS's. On an MSI board 0x33 holds something
        # else, and a byte between 20 and 70 would print as a VRM temperature.
        for name in ("Micro-Star International Co., Ltd. B850MPOWER",
                     "Gigabyte Technology Co., Ltd.", "ASRock Z790", "", None):
            with self.subTest(name=name):
                self.assertFalse(is_asus_board(name))


class VrmMappingTest(unittest.TestCase):
    def setUp(self):
        asus_ec._READER.clear()
        self.addCleanup(asus_ec._READER.clear)

    def test_the_confirmed_reading_decodes_to_what_hwinfo_showed(self):
        register, _minimum, _maximum = CONFIRMED_EC_TEMPERATURES["vrm"]
        self.assertEqual(register, 0x33)
        self.assertEqual(decode_temperature(40), 40)

    def test_the_vrm_is_read_from_its_register(self):
        io = FakeEc({0x33: 43})
        values = read_temperatures(reader_factory=lambda: make_reader(io))
        self.assertEqual(values, {"vrm": 43.0})

    def test_a_reading_outside_the_band_is_dropped(self):
        self.assertIsNone(validate_temperature("vrm", 200.0))
        self.assertIsNone(validate_temperature("vrm", -100.0))
        self.assertEqual(validate_temperature("vrm", 40.0), 40.0)

    def test_an_unknown_sensor_is_dropped(self):
        self.assertIsNone(validate_temperature("nonexistent", 40.0))

    def test_a_negative_reading_is_signed(self):
        self.assertEqual(decode_temperature(0xFF), -1)

    def test_a_dead_controller_is_asked_once(self):
        # Detection drives the EC and Summary refreshes every second.
        attempts = []

        def factory():
            attempts.append(1)
            return make_reader(FakeEc({}, stuck=True))

        for _ in range(5):
            self.assertEqual(read_temperatures(reader_factory=factory), {})
        self.assertEqual(len(attempts), 1)


if __name__ == "__main__":
    unittest.main()
