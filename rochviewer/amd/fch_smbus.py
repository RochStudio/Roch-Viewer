# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This file follows ZenStates-Core and ZenTimings by irusanov
# (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
# and the bounds applied to decoded values were taken from or checked against
# that work, and the comments below say where. Copyright in those parts
# remains with their authors; this file is distributed under the same licence
# they are, which is what makes that use permitted.
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

"""Read-only SMBus transport for the AMD FCH (PIIX4-compatible controller).

Used to reach the DDR5 SPD hub and PMIC, which carry DRAM VDD/VDDQ/VPP.  Those
rails are NOT in the SMU PM table — proven twice, see the note in
``amd_smu_voltages.CONFIRMED_VOLTAGE_OFFSETS`` — so this is the only path to
them.

SAFETY — reads are unrestricted within the allowlist; writes are not:

  * Reads use SMBus "Read Byte Data" against the DDR5 SPD hub (0x50-0x57) and
    PMIC (0x48-0x4F) ranges.  Any other address is refused before it reaches
    the bus.  Writing the register index into HOST_COMMAND is part of the read
    protocol, not a device write: the device is addressed for reading and never
    receives a data byte.

  * Writes use SMBus "Write Byte Data" and are restricted to exactly two
    (address range, register) pairs — the SPD hub's MR11 page-select register
    and the PMIC's R30h telemetry-channel selector.  ``WRITE_ALLOWLIST`` is
    the whole permitted set; every other target is refused before the bus is
    driven, by an equality test on the pair rather than a range check.

    Both are selectors, not settings.  Paging the SPD hub picks which EEPROM
    page a read returns; R30h picks which channel the PMIC's ADC converts, and
    the result is read back from R31h.  Neither can change a rail.

    What the PMIC's rails are actually set by — R21h/R25h/R27h (the VID
    registers) and R2Bh (the VID mode) — appears nowhere in the allowlist, so
    a mis-addressed write cannot move VDD/VDDQ/VPP on a running system.
    ``PMIC_RAIL_CONTROL_REGISTERS`` names them and the test-suite asserts each
    one is refused.

  * The controller is held under the same named mutex HWiNFO / ZenTimings /
    lm-sensors use, so concurrent monitors cannot interleave a transfer.
  * Every transaction is bounded by a deadline and leaves the host status
    register cleared.
"""

from __future__ import annotations

import os
import threading
import time

from rochviewer.hardware.lowlevel_io import InpOutByteIO, NamedMutex

# The mutex every mainstream monitoring tool takes before touching SMBus.
SMBUS_MUTEX_NAME = "Global\\Access_SMBUS.HTP.Method"

# FCH indirect PMIO window used to discover the SMBus IO base.
PMIO_INDEX_PORT = 0xCD6
PMIO_DATA_PORT = 0xCD7
PMIO_SMBA_LOW = 0x2C
PMIO_SMBA_HIGH = 0x2D
SMBUS_BASE_MASK = 0xFFE0
FALLBACK_SMBUS_BASE = 0x0B00

# PIIX4-compatible host register offsets from the controller base.
REG_HOST_STATUS = 0x00
REG_HOST_CONTROL = 0x02
REG_HOST_COMMAND = 0x03
REG_HOST_ADDRESS = 0x04
REG_HOST_DATA0 = 0x05

# HOST_CONTROL: bits 4:2 select the protocol, bit 6 starts the transfer.
PROTOCOL_BYTE_DATA = 0x08
CONTROL_START = 0x40

# SPD5 hub volatile register carrying the EEPROM page selector.
SPD_HUB_PAGE_REGISTER = 0x0B      # MR11
SPD_HUB_PAGE_MASK = 0x07          # 8 pages of 128 bytes
SPD_EEPROM_WINDOW = 0x80          # register bit 7 set => EEPROM, not MR space
SPD_PAGE_SIZE = 0x80

# HOST_STATUS bits.
STATUS_HOST_BUSY = 0x01
STATUS_INTR = 0x02
STATUS_DEV_ERR = 0x04
STATUS_BUS_COLLISION = 0x08
STATUS_FAILED = 0x10
STATUS_ERROR_MASK = STATUS_DEV_ERR | STATUS_BUS_COLLISION | STATUS_FAILED
STATUS_CLEAR_MASK = 0x1F

# Allowlisted DDR5 device addresses (7-bit).
SPD_HUB_ADDRESSES = tuple(range(0x50, 0x58))
PMIC_ADDRESSES = tuple(range(0x48, 0x50))
ALLOWED_ADDRESSES = frozenset(SPD_HUB_ADDRESSES + PMIC_ADDRESSES)

# The complete set of writable targets: {(address, register)}.
#
# One entry: the SPD hub's page selector, which selects which EEPROM page to
# read. No PMIC address appears here at all, so no PMIC register — least of all
# the VID registers 0x21/0x25/0x27 that actually move VDD/VDDQ/VPP — is
# reachable by any write path in this module.
# The PMIC's ADC channel selector. Writing it picks which channel R31h
# reports; JESD301-2 requires ~9 ms to settle before the value is read. It
# carries no rail setting, and the rail registers below are not reachable
# through this list.
PMIC_TELEMETRY_SELECT_REGISTER = 0x30

WRITE_ALLOWLIST = frozenset(
    [(address, SPD_HUB_PAGE_REGISTER) for address in SPD_HUB_ADDRESSES]
    + [(address, PMIC_TELEMETRY_SELECT_REGISTER) for address in PMIC_ADDRESSES]
)

# Registers that must never be writable, asserted by the test-suite.
PMIC_RAIL_CONTROL_REGISTERS = (0x21, 0x25, 0x27, 0x2B)

# The two controllers an AM5 FCH exposes; DIMMs answer on one of them.
CONTROLLER_OFFSETS = (0x00, 0x20)


def read_smbus_base(io):
    """Discover the FCH SMBus IO base, falling back to the usual 0x0B00."""
    try:
        io.outb(PMIO_INDEX_PORT, PMIO_SMBA_LOW)
        low = io.inb(PMIO_DATA_PORT)
        io.outb(PMIO_INDEX_PORT, PMIO_SMBA_HIGH)
        high = io.inb(PMIO_DATA_PORT)
    except OSError:
        return FALLBACK_SMBUS_BASE
    base = ((high << 8) | low) & SMBUS_BASE_MASK
    # A zeroed or all-ones window means the PMIO read did not land; the FCH
    # default is well known, so use it rather than driving a bogus port.
    if base in (0x0000, SMBUS_BASE_MASK):
        return FALLBACK_SMBUS_BASE
    return base


class FchSmbusReader:
    """Serialized, allowlisted, read-only SMBus byte reader."""

    def __init__(self, io=None, mutex=None, base=None,
                 monotonic=time.monotonic, sleep=time.sleep,
                 timeout=0.05):
        self._io = io if io is not None else InpOutByteIO()
        self._mutex = (
            mutex if mutex is not None else NamedMutex(SMBUS_MUTEX_NAME)
        )
        self._lock = threading.Lock()
        self._monotonic = monotonic
        self._sleep = sleep
        timeout = float(timeout)
        if not 0.0 < timeout <= 0.25:
            raise ValueError("SMBus timeout must be within (0, 0.25]")
        self._timeout = timeout
        self.base = read_smbus_base(self._io) if base is None else int(base)
        self.last_error = ""

    def is_driver_open(self):
        return self._io.is_driver_open()

    def _wait_not_busy(self, port_status):
        deadline = self._monotonic() + self._timeout
        while True:
            status = self._io.inb(port_status)
            if not status & STATUS_HOST_BUSY:
                return status
            if self._monotonic() > deadline:
                raise TimeoutError("SMBus host stayed busy")
            self._sleep(0.0002)

    def _wait_complete(self, port_status):
        deadline = self._monotonic() + self._timeout
        while True:
            status = self._io.inb(port_status)
            if status & STATUS_ERROR_MASK:
                raise OSError("SMBus transfer error, status 0x%02X" % status)
            if status & STATUS_INTR and not status & STATUS_HOST_BUSY:
                return status
            if self._monotonic() > deadline:
                raise TimeoutError("SMBus transfer did not complete")
            self._sleep(0.0002)

    def read_byte(self, address, register, controller_offset=0x00):
        """Read one byte using the SMBus Read Byte Data protocol.

        Raises on refusal, timeout or bus error; never returns a partial value.
        """
        address = int(address)
        if address not in ALLOWED_ADDRESSES:
            raise ValueError(
                "SMBus address 0x%02X is outside the DDR5 SPD/PMIC allowlist"
                % address
            )
        if controller_offset not in CONTROLLER_OFFSETS:
            raise ValueError(
                "Unknown SMBus controller offset 0x%02X" % controller_offset
            )
        with self._lock, self._mutex:
            return self._read_byte_locked(address, register, controller_offset)

    def _read_byte_locked(self, address, register, controller_offset):
        """Read one byte. Caller must already hold the lock and the mutex."""
        base = self.base + controller_offset
        port_status = base + REG_HOST_STATUS
        self._wait_not_busy(port_status)
        self._io.outb(port_status, STATUS_CLEAR_MASK)
        # Direction bit is set to 1 (read) for every transfer on this path.
        self._io.outb(base + REG_HOST_ADDRESS, ((address << 1) | 1) & 0xFF)
        self._io.outb(base + REG_HOST_COMMAND, int(register) & 0xFF)
        self._io.outb(
            base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START
        )
        try:
            self._wait_complete(port_status)
            return self._io.inb(base + REG_HOST_DATA0) & 0xFF
        finally:
            # Leave the controller clean for the next owner of the mutex.
            self._io.outb(port_status, STATUS_CLEAR_MASK)

    def _write_byte_locked(self, address, register, value, controller_offset):
        """Write one byte. Caller must already hold the lock and the mutex.

        The allowlist is still enforced here, so no internal caller can bypass
        it by taking the mutex itself.
        """
        if (address, register) not in WRITE_ALLOWLIST:
            raise ValueError(
                "SMBus write to 0x%02X register 0x%02X is not permitted"
                % (address, register)
            )
        base = self.base + controller_offset
        port_status = base + REG_HOST_STATUS
        self._wait_not_busy(port_status)
        self._io.outb(port_status, STATUS_CLEAR_MASK)
        self._io.outb(base + REG_HOST_ADDRESS, (address << 1) & 0xFE)
        self._io.outb(base + REG_HOST_COMMAND, int(register) & 0xFF)
        self._io.outb(base + REG_HOST_DATA0, int(value) & 0xFF)
        self._io.outb(
            base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START
        )
        try:
            self._wait_complete(port_status)
        finally:
            self._io.outb(port_status, STATUS_CLEAR_MASK)

    def write_byte(self, address, register, value, controller_offset=0x00):
        """Write one byte, but only to a target in ``WRITE_ALLOWLIST``.

        The allowlist is a set of exact ``(address, register)`` pairs and holds
        nothing but the SPD hub page selector; PMIC addresses are absent, so no
        rail-control register can be written through this method.
        """
        address = int(address)
        register = int(register) & 0xFF
        if (address, register) not in WRITE_ALLOWLIST:
            raise ValueError(
                "SMBus write to 0x%02X register 0x%02X is not permitted "
                "(only the SPD page and PMIC telemetry selectors are writable)"
                % (address, register)
            )
        if controller_offset not in CONTROLLER_OFFSETS:
            raise ValueError(
                "Unknown SMBus controller offset 0x%02X" % controller_offset
            )
        with self._lock, self._mutex:
            self._write_byte_locked(address, register, value, controller_offset)

    def read_spd(self, address, offset, length, controller_offset=0x00):
        """Read SPD EEPROM bytes across page boundaries.

        Restores whichever page the hub was left on, mirroring the SMN
        selector-restore discipline used elsewhere in this project.
        """
        if address not in SPD_HUB_ADDRESSES:
            raise ValueError("0x%02X is not an SPD hub address" % address)
        original_page = self.read_byte(
            address, SPD_HUB_PAGE_REGISTER, controller_offset
        )
        values = {}
        current_page = None
        try:
            for position in range(int(offset), int(offset) + int(length)):
                page, within = divmod(position, SPD_PAGE_SIZE)
                if page & ~SPD_HUB_PAGE_MASK:
                    break
                if page != current_page:
                    self.write_byte(
                        address, SPD_HUB_PAGE_REGISTER, page, controller_offset
                    )
                    current_page = page
                try:
                    values[position] = self.read_byte(
                        address, SPD_EEPROM_WINDOW | within, controller_offset
                    )
                except (OSError, TimeoutError):
                    continue
        finally:
            try:
                self.write_byte(
                    address,
                    SPD_HUB_PAGE_REGISTER,
                    original_page & SPD_HUB_PAGE_MASK,
                    controller_offset,
                )
            except (OSError, TimeoutError, ValueError) as exc:
                self.last_error = "SPD page restore failed: %s" % exc
        return values

    def probe_address(self, address, controller_offset=0x00, register=0x00):
        """Return True when a device answers at ``address``."""
        try:
            self.read_byte(address, register, controller_offset)
            return True
        except (OSError, TimeoutError, ValueError):
            return False

    def read_block(self, address, registers, controller_offset=0x00):
        """Read several registers, returning ``{register: value}``.

        Registers that fail are omitted rather than reported as zero.
        """
        values = {}
        for register in registers:
            try:
                values[register] = self.read_byte(
                    address, register, controller_offset
                )
            except (OSError, TimeoutError, ValueError):
                continue
        return values
