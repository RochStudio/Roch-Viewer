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

"""Cover Intel PCH SMBus discovery, the read-only contract and the allowlist."""

import unittest

from rochviewer.intel import intel_pch_smbus
from rochviewer.hardware.pci_mcfg import McfgAllocation, ecam_address
from rochviewer.intel.intel_pch_smbus import (
    ALLOWED_ADDRESSES,
    CONTROL_START,
    PROTOCOL_WORD_DATA,
    REG_HOST_ADDRESS,
    REG_HOST_COMMAND,
    REG_HOST_CONTROL,
    REG_HOST_DATA0,
    REG_HOST_DATA1,
    REG_HOST_STATUS,
    STATUS_CLEAR_MASK,
    STATUS_DEV_ERR,
    STATUS_INTR,
    PchSmbusReader,
    SmbusUnavailable,
    find_smbus_base,
)

ALLOCATION = McfgAllocation(0xC0000000, 0, 0, 255)
BASE = 0xEFA0

# What the Z790 target actually reports.
GOOD_FUNCTION_4 = {
    0x00: 0x7A238086,   # Intel vendor, SMBus device
    0x04: 0x00000003,   # I/O space enabled
    0x08: 0x0C050004,   # class 0x0C05 in the upper half
    0x20: 0x0000EFA1,   # BAR4, I/O space flag set
}
# Function 3 on this chipset is audio, which must be rejected on class.
AUDIO_FUNCTION_3 = {
    0x00: 0x7A508086,
    0x04: 0x00000003,
    0x08: 0x04030004,
    0x20: 0xFFF00004,
}


def make_read_dword(by_function):
    """Map ECAM addresses back to (function, offset) for a fake config space."""
    table = {}
    for function, registers in by_function.items():
        for offset, value in registers.items():
            address = ecam_address(ALLOCATION, 0, 0x1F, function, offset)
            table[address] = value

    def read_dword(address):
        if address not in table:
            return 0xFFFFFFFF
        return table[address]

    return read_dword


class NullMutex:
    """Re-enterable stand-in: the real reader takes the mutex once per read."""

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        return False


class _FakeIo:
    """Records every port write so the read-only contract can be asserted."""

    def __init__(self, status_sequence=None, data=(0x00, 0x00)):
        self.writes = []
        self.data = data
        self._status = list(
            status_sequence if status_sequence is not None else [0x00, STATUS_INTR]
        )
        self.driver_open = True

    def is_driver_open(self):
        return self.driver_open

    def inb(self, port):
        if port == BASE + REG_HOST_STATUS:
            return self._status.pop(0) if self._status else STATUS_INTR
        if port == BASE + REG_HOST_DATA0:
            return self.data[0]
        if port == BASE + REG_HOST_DATA1:
            return self.data[1]
        return 0x00

    def outb(self, port, value):
        self.writes.append((port, value))

    def inl(self, port):
        raise AssertionError("this transport must not touch the legacy ports")

    def outl(self, port, value):
        raise AssertionError("this transport must not touch the legacy ports")


def make_reader(io, **kwargs):
    return PchSmbusReader(
        io=io, mutex=NullMutex(), base=BASE, sleep=lambda _s: None, **kwargs
    )


class Ddr5ByteReadTest(unittest.TestCase):
    """The Byte Data path that reaches the DDR5 PMIC and SPD hub."""

    def test_a_pmic_register_is_read_with_the_byte_protocol(self):
        io = _FakeIo(data=(0x8C, 0x00))
        # 0x48 R21h is SWA, the VDD rail: 0x8C decodes to 1.500 V on the bench.
        self.assertEqual(make_reader(io).read_byte(0x48, 0x21), 0x8C)
        control = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_CONTROL
        )
        self.assertEqual(
            control, intel_pch_smbus.PROTOCOL_BYTE_DATA | CONTROL_START
        )

    def test_the_direction_bit_is_always_read(self):
        io = _FakeIo(data=(0x8C, 0x00))
        make_reader(io).read_byte(0x50, 0x31)
        slave = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_ADDRESS
        )
        self.assertEqual(slave & 0x01, 0x01)
        self.assertEqual(slave >> 1, 0x50)

    def test_addresses_outside_the_ddr5_list_are_refused(self):
        io = _FakeIo()
        reader = make_reader(io)
        for address in (0x00, 0x18, 0x1F, 0x47, 0x58, 0x77):
            with self.subTest(address=address):
                with self.assertRaises(ValueError):
                    reader.read_byte(address, 0x21)
        self.assertEqual(io.writes, [])

    def test_an_unknown_controller_offset_is_refused(self):
        io = _FakeIo()
        # 0x20 is the second AM5 controller; the PCH has no such thing, and
        # accepting it silently would read controller zero instead.
        with self.assertRaises(ValueError):
            make_reader(io).read_byte(0x48, 0x21, 0x20)
        self.assertEqual(io.writes, [])

    def test_the_two_allowlists_stay_separate(self):
        # Widening one device class must never widen the other.
        self.assertEqual(ALLOWED_ADDRESSES, frozenset(range(0x18, 0x20)))
        self.assertEqual(
            intel_pch_smbus.DDR5_ADDRESSES, frozenset(range(0x48, 0x58))
        )
        self.assertFalse(ALLOWED_ADDRESSES & intel_pch_smbus.DDR5_ADDRESSES)

    def test_a_thermal_sensor_is_unreachable_from_the_byte_path(self):
        # The JC-42.4 sensors answer Word Data; keeping them off this path is
        # what stops the two allowlists collapsing into one over time.
        with self.assertRaises(ValueError):
            make_reader(_FakeIo()).read_byte(0x18, 0x05)

    def test_the_controller_is_left_clean(self):
        io = _FakeIo(data=(0x8C, 0x00))
        make_reader(io).read_byte(0x48, 0x21)
        status_writes = [
            value for port, value in io.writes
            if port == BASE + REG_HOST_STATUS
        ]
        self.assertEqual(status_writes[-1], STATUS_CLEAR_MASK)

    def test_a_bus_error_raises_and_still_clears_status(self):
        io = _FakeIo(status_sequence=[0x00, STATUS_DEV_ERR], data=(0x00, 0x00))
        with self.assertRaises(OSError):
            make_reader(io).read_byte(0x48, 0x21)
        status_writes = [
            value for port, value in io.writes
            if port == BASE + REG_HOST_STATUS
        ]
        self.assertEqual(status_writes[-1], STATUS_CLEAR_MASK)

    def test_only_controller_registers_are_written(self):
        io = _FakeIo(data=(0x8C, 0x00))
        make_reader(io).read_byte(0x48, 0x21)
        written = {port - BASE for port, _value in io.writes}
        self.assertEqual(
            written,
            {REG_HOST_STATUS, REG_HOST_ADDRESS, REG_HOST_COMMAND,
             REG_HOST_CONTROL},
        )


class BaseDiscoveryTest(unittest.TestCase):
    def test_the_smbus_function_is_found_and_its_bar_masked(self):
        read_dword = make_read_dword({4: GOOD_FUNCTION_4, 3: AUDIO_FUNCTION_3})
        self.assertEqual(
            find_smbus_base(read_dword, NullMutex(), ALLOCATION), BASE
        )

    def test_a_non_intel_vendor_is_refused(self):
        registers = {**GOOD_FUNCTION_4, 0x00: 0x7A231022}
        read_dword = make_read_dword({4: registers})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)

    def test_a_function_with_the_wrong_class_is_refused(self):
        read_dword = make_read_dword({4: AUDIO_FUNCTION_3})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)

    def test_function_three_is_used_when_it_is_the_smbus_one(self):
        # Older chipsets put the controller on function 3.
        read_dword = make_read_dword({3: GOOD_FUNCTION_4})
        self.assertEqual(
            find_smbus_base(read_dword, NullMutex(), ALLOCATION), BASE
        )

    def test_disabled_io_space_is_refused(self):
        registers = {**GOOD_FUNCTION_4, 0x04: 0x00000002}
        read_dword = make_read_dword({4: registers})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)

    def test_a_memory_bar_is_refused(self):
        registers = {**GOOD_FUNCTION_4, 0x20: 0x0000EFA0}
        read_dword = make_read_dword({4: registers})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)

    def test_an_unassigned_bar_is_refused(self):
        registers = {**GOOD_FUNCTION_4, 0x20: 0x00000001}
        read_dword = make_read_dword({4: registers})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)

    def test_an_absent_device_is_refused(self):
        read_dword = make_read_dword({})
        with self.assertRaises(SmbusUnavailable):
            find_smbus_base(read_dword, NullMutex(), ALLOCATION)


class ReadOnlyContractTest(unittest.TestCase):
    """The module must not be able to write to a device on the bus."""

    def test_the_write_allowlist_is_one_selector_on_pmic_addresses(self):
        # The whole permitted set for write_byte. The SPD hubs are written
        # too, but not through here -- select_spd_page is the only path to
        # them, and it cannot be pointed at a register of the caller's
        # choosing. Anything beyond the ADC channel selector appearing here
        # means a widening that was not reviewed.
        self.assertEqual(
            intel_pch_smbus.WRITE_ALLOWLIST,
            frozenset(
                (address, 0x30)
                for address in intel_pch_smbus.PMIC_ADDRESSES
            ),
        )

    def test_the_only_writable_pmic_register_is_the_adc_selector(self):
        for address, register in intel_pch_smbus.WRITE_ALLOWLIST:
            if address in intel_pch_smbus.PMIC_ADDRESSES:
                self.assertEqual(
                    register, intel_pch_smbus.PMIC_TELEMETRY_SELECT_REGISTER
                )

    def test_no_rail_control_register_is_writable(self):
        io = _FakeIo()
        reader = make_reader(io)
        for address in intel_pch_smbus.PMIC_ADDRESSES:
            for register in intel_pch_smbus.PMIC_RAIL_CONTROL_REGISTERS:
                with self.subTest(address=address, register=register):
                    self.assertNotIn(
                        (address, register), intel_pch_smbus.WRITE_ALLOWLIST
                    )
                    with self.assertRaises(ValueError):
                        reader.write_byte(address, register, 0x00)
        self.assertEqual(io.writes, [])

    def test_no_spd_hub_register_is_reachable_through_write_byte(self):
        # Including the page register: the hubs are not in the allowlist at
        # all, so the general write path cannot touch them.
        io = _FakeIo()
        reader = make_reader(io)
        for address in intel_pch_smbus.SPD_HUB_ADDRESSES:
            for register in (0x00, 0x0A, 0x0B, 0x0C, 0x30, 0x80, 0x8B):
                with self.subTest(address=address, register=register):
                    with self.assertRaises(ValueError):
                        reader.write_byte(address, register, 0x00)
        self.assertEqual(io.writes, [])

    def test_the_page_selector_cannot_be_aimed_at_another_register(self):
        # The register is not a parameter. That is the whole safety property:
        # no caller, and no bug in a caller, can steer this at the EEPROM
        # window where a write would corrupt the module's SPD.
        io = _FakeIo()
        make_reader(io).select_spd_page(0x50, 0x04)
        commands = [value for port, value in io.writes
                    if port == BASE + REG_HOST_COMMAND]
        self.assertEqual(commands, [intel_pch_smbus.SPD_HUB_PAGE_REGISTER])

    def test_the_page_selector_refuses_a_non_hub_address(self):
        io = _FakeIo()
        reader = make_reader(io)
        for address in (0x00, 0x18, 0x48, 0x4A, 0x58, 0x77):
            with self.subTest(address=address):
                with self.assertRaises(ValueError):
                    reader.select_spd_page(address, 0x04)
        self.assertEqual(io.writes, [])

    def test_the_page_value_is_masked_to_the_three_page_bits(self):
        # A caller passing a byte-wide value must not be able to set anything
        # else in MR11 -- the addressing-mode bits live in the same register.
        io = _FakeIo()
        make_reader(io).select_spd_page(0x50, 0xFF)
        data0 = [value for port, value in io.writes
                 if port == BASE + REG_HOST_DATA0]
        self.assertEqual(data0, [intel_pch_smbus.SPD_HUB_PAGE_MASK])

    def test_the_page_select_is_a_process_call_with_the_direction_bit_set(self):
        # Both halves matter and were measured on the bench: a byte write is
        # refused outright while the platform's SPD Write Disable is armed,
        # and the interlock classifies on the direction bit, so the bit has to
        # be set for the write phase to reach the hub. This is the transaction
        # CPU-Z issues for the same purpose.
        io = _FakeIo()
        make_reader(io).select_spd_page(0x50, 0x04)
        by_port = {}
        for port, value in io.writes:
            by_port.setdefault(port, []).append(value)
        self.assertEqual(
            by_port[BASE + REG_HOST_CONTROL],
            [intel_pch_smbus.PROTOCOL_PROC_CALL | CONTROL_START],
        )
        self.assertEqual(by_port[BASE + REG_HOST_ADDRESS], [(0x50 << 1) | 1])
        # The second data byte is what would land in the write-protection
        # register if the hub consumed it; it is always zero.
        self.assertEqual(by_port[BASE + REG_HOST_DATA1], [0x00])

    def test_read_spd_restores_the_page_the_hub_was_left_on(self):
        class PagingIo(_FakeIo):
            def __init__(self):
                _FakeIo.__init__(self, status_sequence=[])
                self.page_writes = []

            def outb(self, port, value):
                _FakeIo.outb(self, port, value)
                if port == BASE + REG_HOST_DATA0:
                    self.page_writes.append(value)

        io = PagingIo()
        io.data = (0x03, 0x00)      # hub reports it is currently on page 3
        make_reader(io).read_spd(0x50, 0x200, 8)
        self.assertEqual(io.page_writes[0], 0x04)
        # Page 4 holds 0x200; the hub is put back on 3 afterwards.
        self.assertEqual(io.page_writes, [0x04, 0x03])

    def test_read_spd_holds_the_bus_for_the_whole_sequence(self):
        # The EEPROM window returns bytes from whichever page an earlier
        # transaction selected, so the page select and the reads that depend
        # on it have to be one critical section. Taking the mutex per byte
        # lets another master repage the hub mid-read, which on the bench
        # returned a part number ending "Kz>>>}" -- right offsets, wrong page.
        class CountingMutex:
            def __init__(self):
                self.acquisitions = 0
                self.depth = 0
                self.max_depth = 0

            def __enter__(self):
                self.acquisitions += 1
                self.depth += 1
                self.max_depth = max(self.max_depth, self.depth)
                return self

            def __exit__(self, *exc_info):
                self.depth -= 1
                return False

        mutex = CountingMutex()
        io = _FakeIo(status_sequence=[])
        reader = PchSmbusReader(
            io=io, mutex=mutex, base=BASE, sleep=lambda _s: None
        )
        reader.read_spd(0x50, 0x200, 48)
        self.assertEqual(mutex.acquisitions, 1)
        self.assertEqual(mutex.max_depth, 1, "the mutex was re-entered")

    def test_read_spd_refuses_an_address_that_is_not_an_spd_hub(self):
        io = _FakeIo()
        with self.assertRaises(ValueError):
            make_reader(io).read_spd(0x48, 0x200, 8)
        self.assertEqual(io.writes, [])

    def test_the_allowlisted_write_drives_the_bus_with_direction_clear(self):
        io = _FakeIo()
        make_reader(io).write_byte(0x48, 0x30, 0x80)
        slave = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_ADDRESS
        )
        self.assertEqual(slave & 0x01, 0x00)
        self.assertEqual(slave >> 1, 0x48)
        data = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_DATA0
        )
        self.assertEqual(data, 0x80)

    def test_a_write_leaves_the_controller_clean(self):
        io = _FakeIo()
        make_reader(io).write_byte(0x48, 0x30, 0x80)
        status_writes = [
            value for port, value in io.writes
            if port == BASE + REG_HOST_STATUS
        ]
        self.assertEqual(status_writes[-1], STATUS_CLEAR_MASK)

    def test_reads_never_clear_the_direction_bit(self):
        # Reads and the write are separate primitives; no argument to a read
        # can turn it into a write.
        for call in (
            lambda r: r.read_word_bytes(0x19, 0x05),
            lambda r: r.read_byte(0x48, 0x21),
            lambda r: r.read_byte(0x50, 0x31),
        ):
            io = _FakeIo(data=(0xC1, 0xE8))
            with self.subTest(call=call):
                call(make_reader(io))
                slave = next(
                    value for port, value in io.writes
                    if port == BASE + REG_HOST_ADDRESS
                )
                self.assertEqual(slave & 0x01, 0x01)

    def test_no_write_word_primitive_exists(self):
        source = open(intel_pch_smbus.__file__, encoding="utf-8").read()
        for forbidden in ("PROTOCOL_BYTE_WRITE", "write_word"):
            with self.subTest(forbidden=forbidden):
                self.assertNotIn(forbidden, source)

    def test_the_direction_bit_is_always_read(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        make_reader(io).read_word_bytes(0x19, 0x05)
        slave = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_ADDRESS
        )
        self.assertEqual(slave & 0x01, 0x01)
        self.assertEqual(slave >> 1, 0x19)

    def test_spd_eeprom_addresses_are_unreachable(self):
        io = _FakeIo()
        reader = make_reader(io)
        for address in range(0x50, 0x58):
            with self.subTest(address=address):
                with self.assertRaises(ValueError):
                    reader.read_word_bytes(address, 0x00)
        self.assertEqual(io.writes, [])

    def test_only_thermal_sensor_addresses_are_allowed(self):
        self.assertEqual(ALLOWED_ADDRESSES, frozenset(range(0x18, 0x20)))

    def test_only_controller_registers_are_written(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        make_reader(io).read_word_bytes(0x19, 0x05)
        written = {port - BASE for port, _value in io.writes}
        self.assertEqual(
            written,
            {REG_HOST_STATUS, REG_HOST_ADDRESS, REG_HOST_COMMAND,
             REG_HOST_CONTROL},
        )


class TransactionTest(unittest.TestCase):
    def test_a_word_read_issues_the_word_protocol_and_start(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        make_reader(io).read_word_bytes(0x19, 0x05)
        control = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_CONTROL
        )
        self.assertEqual(control, PROTOCOL_WORD_DATA | CONTROL_START)

    def test_the_register_is_sent_as_the_command_byte(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        make_reader(io).read_word_bytes(0x19, 0x05)
        command = next(
            value for port, value in io.writes
            if port == BASE + REG_HOST_COMMAND
        )
        self.assertEqual(command, 0x05)

    def test_bytes_come_back_in_wire_order(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        self.assertEqual(make_reader(io).read_word_bytes(0x19, 0x05), (0xC1, 0xE8))

    def test_status_is_cleared_before_and_after(self):
        io = _FakeIo(data=(0xC1, 0xE8))
        make_reader(io).read_word_bytes(0x19, 0x05)
        clears = [
            value for port, value in io.writes
            if port == BASE + REG_HOST_STATUS
        ]
        self.assertEqual(clears, [STATUS_CLEAR_MASK, STATUS_CLEAR_MASK])

    def test_a_bus_error_raises_and_still_clears_status(self):
        io = _FakeIo(status_sequence=[0x00, STATUS_DEV_ERR], data=(0, 0))
        with self.assertRaises(OSError):
            make_reader(io).read_word_bytes(0x19, 0x05)
        self.assertEqual(io.writes[-1], (BASE + REG_HOST_STATUS, STATUS_CLEAR_MASK))

    def test_a_stuck_bus_times_out_rather_than_hanging(self):
        io = _FakeIo(status_sequence=[0x01] * 4000, data=(0, 0))
        ticks = iter([0.0] + [1.0] * 100)
        reader = PchSmbusReader(
            io=io, mutex=NullMutex(), base=BASE,
            monotonic=lambda: next(ticks), sleep=lambda _s: None,
        )
        with self.assertRaises(TimeoutError):
            reader.read_word_bytes(0x19, 0x05)

    def test_an_out_of_range_timeout_is_refused(self):
        for bad in (0.0, -1.0, 0.5):
            with self.subTest(timeout=bad):
                with self.assertRaises(ValueError):
                    PchSmbusReader(io=_FakeIo(), mutex=NullMutex(),
                                   base=BASE, timeout=bad)


class ScanTest(unittest.TestCase):
    def test_only_answering_addresses_are_returned(self):
        answering = {0x19, 0x1B}

        class _ScanIo(_FakeIo):
            def __init__(self):
                super().__init__()
                self.current = None

            def inb(self, port):
                if port == BASE + REG_HOST_STATUS:
                    if self.current in answering:
                        return STATUS_INTR
                    return STATUS_DEV_ERR
                return 0x00

            def outb(self, port, value):
                if port == BASE + REG_HOST_ADDRESS:
                    self.current = value >> 1
                super().outb(port, value)

        reader = make_reader(_ScanIo())
        self.assertEqual(reader.responding_addresses(0x05), (0x19, 0x1B))


if __name__ == "__main__":
    unittest.main()

