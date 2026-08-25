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

"""Read-only Intel PCH SMBus access, scoped to DDR4 temperature sensors.

Fourth transport in this project. The DIMM temperature sensor answers nowhere
else: it is a device on the board's SMBus, not a register in the memory
controller, and the Super I/O sensor block does not carry it either.

The host controller is register-compatible with the AMD FCH one this project
already drives, so the transaction loop is the ordinary PIIX4 one. Only the
base-address discovery differs: AMD publishes it through the PMIO index/data
ports, Intel through the SMBus function's own PCI configuration space.

That configuration space is reached through the firmware MCFG/ECAM window
rather than the legacy CF8/CFC ports, for the same reason the AM5 backend
does: measured on the Z790 target, CF8/CFC returned 0xFFFFFFFF for every
device including the 00:00.0 host bridge, which always exists. The ports are
not a usable transport here, so they are not used at all.

SAFETY - this module reads, plus one allowlisted selector write:

  * Every read assembles the transmit-slave-address byte with the read
    direction bit set, from a code path that has no direction parameter. Reads
    and the write are separate primitives, so no caller can turn a read into a
    write by passing an argument.
  * Two device classes, two read protocols, two allowlists, so widening one
    cannot widen the other. Word Data reaches the JEDEC thermal sensors at
    0x18-0x1F; Byte Data reaches the DDR5 PMIC and SPD hub at 0x48-0x57.
  * Exactly one writable target exists: the PMIC's ADC channel selector at
    R30h, on PMIC addresses only. ``WRITE_ALLOWLIST`` is the whole permitted
    set, tested as an equality on the ``(address, register)`` pair rather than
    a range. R30h is a selector and not a setting -- it picks which channel
    R31h converts, and cannot move a rail. The VID registers 0x21/0x25/0x27
    that do move VDD/VDDQ/VPP, and the mode register 0x2B, appear in no
    allowlist and cannot be written from here.
  * The SPD hubs at 0x50-0x57 take exactly one write, and not through
    ``write_byte``, which refuses them outright: ``select_spd_page`` names the
    page register itself rather than accepting one, so nothing can steer a
    write at the EEPROM window, where it would corrupt a module's SPD.
  * That write exists because the PMIC's measured rails cannot be reached
    without it. The ADC is a multiplexer: R30h selects a channel and R31h
    holds the sample, so a read-only transport can report what the rails are
    *set* to and never what they measure.
  * Access is serialised on the named mutexes the mainstream monitoring tools
    take for SMBus and for PCI configuration space.
  * The controller's status register is cleared on the way out, including on
    error, so the next owner of the mutex inherits a clean controller.
"""

from __future__ import annotations

import threading
import time

# Mutex the mainstream monitoring tools take before driving SMBus.
SMBUS_MUTEX_NAME = "Global\\Access_SMBUS.HTP.Method"
PCI_MUTEX_NAME = "Global\\Access_PCI"

# The SMBus controller's fixed location. 100-series and later put it on
# function 4; older chipsets used function 3, so both are tried and each is
# identity-checked before use.
SMBUS_BUS = 0x00
SMBUS_DEVICE = 0x1F
SMBUS_FUNCTIONS = (4, 3)

INTEL_VENDOR_ID = 0x8086
# PCI base class 0x0C (serial bus), sub-class 0x05 (SMBus).
SMBUS_CLASS_CODE = 0x0C05

PCI_VENDOR_OFFSET = 0x00
PCI_COMMAND_OFFSET = 0x04
PCI_CLASS_OFFSET = 0x08
PCI_BAR4_OFFSET = 0x20

PCI_COMMAND_IO_SPACE = 0x0001
BAR_IO_SPACE_FLAG = 0x0001
SMBUS_BASE_MASK = 0xFFE0

# Host controller register offsets, identical to the FCH layout.
REG_HOST_STATUS = 0x00
REG_HOST_CONTROL = 0x02
REG_HOST_COMMAND = 0x03
REG_HOST_ADDRESS = 0x04
REG_HOST_DATA0 = 0x05
REG_HOST_DATA1 = 0x06

# Transfer protocols. Word Data is what a JC-42.4 sensor answers with; Byte
# Data is how a DDR5 PMIC and SPD hub answer, and is also what the one
# allowlisted write uses. Read and write differ by the direction bit in the
# address byte, not by the protocol code, so there is no separate
# write-protocol constant to widen.
PROTOCOL_WORD_DATA = 0x0C
PROTOCOL_BYTE_DATA = 0x08
# Process Call: two bytes out, two back, in one transaction. Used on exactly
# one register; see select_spd_page for why it has to be this protocol.
PROTOCOL_PROC_CALL = 0x10
CONTROL_START = 0x40

STATUS_HOST_BUSY = 0x01
STATUS_INTR = 0x02
STATUS_DEV_ERR = 0x04
STATUS_BUS_COLLISION = 0x08
STATUS_FAILED = 0x10
STATUS_ERROR_MASK = STATUS_DEV_ERR | STATUS_BUS_COLLISION | STATUS_FAILED
STATUS_CLEAR_MASK = 0x1F

# JEDEC JC-42.4 thermal sensors. One address per slot, eight slots maximum.
# This list governs the Word Data path and nothing else.
THERMAL_SENSOR_ADDRESSES = tuple(range(0x18, 0x20))
ALLOWED_ADDRESSES = frozenset(THERMAL_SENSOR_ADDRESSES)

# DDR5 devices, reached with the Byte Data path. Deliberately a separate list
# from ALLOWED_ADDRESSES: different devices, different protocol, different
# allowlist, so neither can be widened by an edit meant for the other.
#
# A DDR5 module does not carry a JC-42.4 sensor at all -- the thermal sensor
# moved inside the SPD5 hub, which is why the 0x18-0x1F sweep finds nothing on
# a DDR5 board and the DIMM rows above it stay blank.
SPD_HUB_ADDRESSES = tuple(range(0x50, 0x58))
PMIC_ADDRESSES = tuple(range(0x48, 0x50))
DDR5_ADDRESSES = frozenset(SPD_HUB_ADDRESSES + PMIC_ADDRESSES)

# The PCH exposes one SMBus controller where an AM5 FCH exposes two. Named and
# accepted so a caller written against either transport passes the same
# arguments; anything else is refused rather than silently treated as zero.
CONTROLLER_OFFSETS = (0x00,)

# The PMIC's ADC channel selector.
PMIC_TELEMETRY_SELECT_REGISTER = 0x30

# The SPD5 hub's page selector, MR11, and the geometry it selects. The module
# identity lives at byte 0x200 and the hub exposes 128 bytes at a time, so the
# page has to be selected before the read.
#
# Selecting it is a write, and the PCH may be told to refuse writes here. Its
# SMBus Host Configuration register (D31:F4 offset 0x40) carries SPD Write
# Disable at bit 4, which on this board's BIOS defaults to enabled. The bit is
# set-once: firmware arms it during POST and nothing in the running OS can
# clear it, through either config decode path -- measured on the bench, where
# setting it took effect and clearing it did not.
#
# It gates on the transaction's direction bit rather than on whether bytes
# reach the device, so a Process Call -- which the controller issues with that
# bit set, yet which still carries a write phase -- selects the page with the
# interlock armed. See select_spd_page, which is the only thing here that
# writes to a hub, and which reaches exactly this one register.
SPD_HUB_PAGE_REGISTER = 0x0B
SPD_HUB_PAGE_MASK = 0x07          # 8 pages of 128 bytes
SPD_EEPROM_WINDOW = 0x80          # register bit 7 set => EEPROM, not MR space
SPD_PAGE_SIZE = 0x80

# The complete set of writable targets: {(address, register)}.
#
# One register, on PMIC addresses only: the ADC channel selector, which
# chooses what a following read returns and stores nothing.
#
# The SPD hubs are deliberately absent. They are written -- the page has to be
# selected -- but not through here: that goes through select_spd_page, which
# is fixed to one register and cannot be pointed at the EEPROM window, where a
# stray write would corrupt a module's SPD.
WRITE_ALLOWLIST = frozenset(
    (address, PMIC_TELEMETRY_SELECT_REGISTER) for address in PMIC_ADDRESSES
)

# Registers that must never be writable, asserted by the test suite. The first
# three set VDD/VDDQ/VPP; 0x2B selects which VID encoding they use, which
# moves the same rails by reinterpreting them.
PMIC_RAIL_CONTROL_REGISTERS = (0x21, 0x25, 0x27, 0x2B)


class SmbusUnavailable(RuntimeError):
    """Raised when the controller cannot be located or driven."""


def _default_read_dword():
    """Physical dword reader built on the bundled InpOut mapping."""
    from read import read_physical_memory_int

    def read_dword(address):
        value = read_physical_memory_int(address, 4)
        if value is None:
            raise OSError("physical read failed at 0x%016X" % address)
        return int(value) & 0xFFFFFFFF

    return read_dword


def default_ecam_allocation():
    """Return the firmware's ECAM allocation covering the SMBus controller."""
    from pci_mcfg import get_mcfg_table, parse_mcfg, select_allocation

    return select_allocation(parse_mcfg(get_mcfg_table()), 0, SMBUS_BUS)


def _config_dword(read_dword, allocation, function, offset):
    """Read one dword from the SMBus function's PCI configuration space."""
    from pci_mcfg import ecam_address

    address = ecam_address(
        allocation, SMBUS_BUS, SMBUS_DEVICE, function, offset
    )
    return int(read_dword(address)) & 0xFFFFFFFF


def find_smbus_base(read_dword=None, mutex=None, allocation=None):
    """Return the controller's I/O base, or raise ``SmbusUnavailable``.

    The function is identified before its BAR is trusted: an Intel vendor ID
    and the SMBus class code both have to match, so a chipset that puts
    something else at 00:1F.4 cannot be driven as if it were a bus controller.
    """
    if read_dword is None:
        read_dword = _default_read_dword()
    if allocation is None:
        allocation = default_ecam_allocation()
    if mutex is None:
        from lowlevel_io import NamedMutex

        mutex = NamedMutex(PCI_MUTEX_NAME)

    reasons = []
    with mutex:
        for function in SMBUS_FUNCTIONS:
            identity = _config_dword(
                read_dword, allocation, function, PCI_VENDOR_OFFSET
            )
            if identity in (0xFFFFFFFF, 0x00000000):
                reasons.append("00:1F.%d absent" % function)
                continue
            if (identity & 0xFFFF) != INTEL_VENDOR_ID:
                reasons.append(
                    "00:1F.%d vendor 0x%04X" % (function, identity & 0xFFFF)
                )
                continue

            class_dword = _config_dword(
                read_dword, allocation, function, PCI_CLASS_OFFSET
            )
            if ((class_dword >> 16) & 0xFFFF) != SMBUS_CLASS_CODE:
                reasons.append(
                    "00:1F.%d class 0x%04X"
                    % (function, (class_dword >> 16) & 0xFFFF)
                )
                continue

            command = _config_dword(
                read_dword, allocation, function, PCI_COMMAND_OFFSET
            )
            if not command & PCI_COMMAND_IO_SPACE:
                reasons.append("00:1F.%d I/O space disabled" % function)
                continue

            bar = _config_dword(
                read_dword, allocation, function, PCI_BAR4_OFFSET
            )
            if not bar & BAR_IO_SPACE_FLAG:
                reasons.append("00:1F.%d BAR4 is not I/O" % function)
                continue

            base = bar & SMBUS_BASE_MASK
            if not base:
                reasons.append("00:1F.%d BAR4 unassigned" % function)
                continue
            return base

    raise SmbusUnavailable(
        "No Intel SMBus controller found (%s)" % "; ".join(reasons or ["none"])
    )


class PchSmbusReader:
    """Serialized, allowlisted, read-only SMBus word reader."""

    def __init__(self, io=None, mutex=None, base=None,
                 monotonic=time.monotonic, sleep=time.sleep,
                 timeout=0.05):
        if io is None:
            # Imported here rather than at module scope: these are the
            # project's platform-neutral port-I/O primitives, but they live in
            # a module whose name is AMD's, and the Intel path should not pull
            # that in just by being imported.
            from lowlevel_io import InpOutByteIO

            io = InpOutByteIO()
        self._io = io
        if mutex is None:
            from lowlevel_io import NamedMutex

            mutex = NamedMutex(SMBUS_MUTEX_NAME)
        self._mutex = mutex
        self._lock = threading.Lock()
        self._monotonic = monotonic
        self._sleep = sleep
        timeout = float(timeout)
        if not 0.0 < timeout <= 0.25:
            raise ValueError("SMBus timeout must be within (0, 0.25]")
        self._timeout = timeout
        self.base = find_smbus_base() if base is None else int(base)
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

    def read_word_bytes(self, address, register):
        """Read a register with the Read Word protocol.

        Returns ``(first_byte, second_byte)`` in the order the device put them
        on the wire, leaving byte order to the caller: SMBus sends the low byte
        first, but a JC-42.4 sensor answers most-significant byte first, and
        only the device's own specification says which applies.

        Raises on refusal, timeout or bus error; never returns a partial value.
        """
        address = int(address)
        if address not in ALLOWED_ADDRESSES:
            raise ValueError(
                "SMBus address 0x%02X is outside the thermal-sensor range"
                % address
            )
        with self._lock, self._mutex:
            return self._read_word_locked(address, register)

    def read_byte(self, address, register, controller_offset=0x00):
        """Read a DDR5 device register with the Read Byte Data protocol.

        The signature matches the AM5 transport's so one caller can drive
        either bus without knowing which platform it is on.

        Raises on refusal, timeout or bus error; never returns a partial value.
        """
        address = int(address)
        if address not in DDR5_ADDRESSES:
            raise ValueError(
                "SMBus address 0x%02X is outside the DDR5 SPD/PMIC allowlist"
                % address
            )
        if controller_offset not in CONTROLLER_OFFSETS:
            raise ValueError(
                "Unknown SMBus controller offset 0x%02X" % controller_offset
            )
        with self._lock, self._mutex:
            return self._read_byte_locked(address, register)

    def read_spd(self, address, offset, length, controller_offset=0x00):
        """Read SPD EEPROM bytes across page boundaries.

        Restores whichever page the hub was left on, so a tool reading the
        module after this one finds it as it was.

        The whole sequence holds the bus mutex, rather than taking it per
        byte. It has to: which bytes the EEPROM window returns depends on a
        page selected by an earlier transaction, so releasing the mutex
        between them lets another master repage the hub underneath the read.
        That is not hypothetical -- running this beside a second tool doing
        the same thing returned a part number ending "Kz>>>}", bytes read at
        the right offsets of the wrong page. Per-byte locking is safe only for
        reads that carry their own address.
        """
        if address not in SPD_HUB_ADDRESSES:
            raise ValueError("0x%02X is not an SPD hub address" % address)
        if controller_offset not in CONTROLLER_OFFSETS:
            raise ValueError(
                "Unknown SMBus controller offset 0x%02X" % controller_offset
            )
        values = {}
        with self._lock, self._mutex:
            original_page = self._read_byte_locked(
                address, SPD_HUB_PAGE_REGISTER
            )
            current_page = None
            try:
                for position in range(int(offset), int(offset) + int(length)):
                    page, within = divmod(position, SPD_PAGE_SIZE)
                    if page & ~SPD_HUB_PAGE_MASK:
                        break
                    if page != current_page:
                        # A refusal here is the platform refusing the
                        # transaction outright, not a transient error, so it
                        # stops the read rather than being retried for every
                        # byte of every page.
                        self._select_page_locked(address, page)
                        current_page = page
                    try:
                        values[position] = self._read_byte_locked(
                            address, SPD_EEPROM_WINDOW | within
                        )
                    except (OSError, TimeoutError):
                        continue
            finally:
                try:
                    self._select_page_locked(address, original_page)
                except (OSError, TimeoutError) as exc:
                    self.last_error = "SPD page restore failed: %s" % exc
        return values

    def probe_address(self, address, controller_offset=0x00, register=0x00):
        """Return True when a device answers at ``address``."""
        try:
            self.read_byte(address, register, controller_offset)
            return True
        except (OSError, TimeoutError, ValueError):
            return False

    def write_byte(self, address, register, value, controller_offset=0x00):
        """Write one byte, but only to a target in ``WRITE_ALLOWLIST``.

        The allowlist holds exactly one register on the PMIC addresses -- the
        ADC channel selector -- so no rail-control register, and no SPD hub
        address at all, is reachable through this method.
        """
        address = int(address)
        register = int(register) & 0xFF
        if (address, register) not in WRITE_ALLOWLIST:
            raise ValueError(
                "SMBus write to 0x%02X register 0x%02X is not permitted "
                "(only the PMIC telemetry selector is writable)"
                % (address, register)
            )
        if controller_offset not in CONTROLLER_OFFSETS:
            raise ValueError(
                "Unknown SMBus controller offset 0x%02X" % controller_offset
            )
        with self._lock, self._mutex:
            self._write_byte_locked(address, register, value)

    def _write_byte_locked(self, address, register, value):
        """Write one byte. Caller must already hold the lock and the mutex."""
        base = self.base
        port_status = base + REG_HOST_STATUS
        self._wait_not_busy(port_status)
        self._io.outb(port_status, STATUS_CLEAR_MASK)
        # The only transfer on this transport that clears the direction bit.
        self._io.outb(base + REG_HOST_ADDRESS, (address << 1) & 0xFE)
        self._io.outb(base + REG_HOST_COMMAND, int(register) & 0xFF)
        self._io.outb(base + REG_HOST_DATA0, int(value) & 0xFF)
        self._io.outb(
            base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START
        )
        try:
            self._wait_complete(port_status)
        finally:
            # Leave the controller clean for the next owner of the mutex.
            self._io.outb(port_status, STATUS_CLEAR_MASK)

    def select_spd_page(self, address, page):
        """Point an SPD hub's EEPROM window at one of its eight pages.

        Fixed to SPD_HUB_PAGE_REGISTER on an SPD hub address: the register is
        not a parameter, so nothing can steer this at the EEPROM window and
        write to the array itself. The value is masked to the three page bits.

        Issued as a Process Call rather than a byte write; see
        SPD_HUB_PAGE_REGISTER for why a byte write cannot reach the hub here.

        A Process Call sends a second data byte. On this hub it goes nowhere:
        dumping MR0-MR20 either side of the call shows MR11 moving and nothing
        else, so the block write-protection registers next to it are untouched.
        """
        address = int(address)
        if address not in SPD_HUB_ADDRESSES:
            raise ValueError(
                "0x%02X is not an SPD hub address" % address
            )
        with self._lock, self._mutex:
            self._select_page_locked(address, page)

    def _select_page_locked(self, address, page):
        """Select a page. Caller must already hold the lock and the mutex."""
        base = self.base
        port_status = base + REG_HOST_STATUS
        self._wait_not_busy(port_status)
        self._io.outb(port_status, STATUS_CLEAR_MASK)
        # Direction bit set: on this path it is also what carries the write
        # past SPD Write Disable.
        self._io.outb(base + REG_HOST_ADDRESS, ((address << 1) | 1) & 0xFF)
        self._io.outb(base + REG_HOST_COMMAND, SPD_HUB_PAGE_REGISTER)
        self._io.outb(base + REG_HOST_DATA0, int(page) & SPD_HUB_PAGE_MASK)
        self._io.outb(base + REG_HOST_DATA1, 0x00)
        self._io.outb(
            base + REG_HOST_CONTROL, PROTOCOL_PROC_CALL | CONTROL_START
        )
        try:
            self._wait_complete(port_status)
        finally:
            # Leave the controller clean for the next owner of the mutex.
            self._io.outb(port_status, STATUS_CLEAR_MASK)

    def _read_byte_locked(self, address, register):
        """Read one byte. Caller must already hold the lock and the mutex."""
        base = self.base
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

    def _read_word_locked(self, address, register):
        """Read one word. Caller must already hold the lock and the mutex."""
        base = self.base
        port_status = base + REG_HOST_STATUS
        self._wait_not_busy(port_status)
        self._io.outb(port_status, STATUS_CLEAR_MASK)
        # Direction bit is set to 1 (read) for every transfer on this path.
        self._io.outb(base + REG_HOST_ADDRESS, ((address << 1) | 1) & 0xFF)
        self._io.outb(base + REG_HOST_COMMAND, int(register) & 0xFF)
        self._io.outb(
            base + REG_HOST_CONTROL, PROTOCOL_WORD_DATA | CONTROL_START
        )
        try:
            self._wait_complete(port_status)
            return (
                self._io.inb(base + REG_HOST_DATA0) & 0xFF,
                self._io.inb(base + REG_HOST_DATA1) & 0xFF,
            )
        finally:
            # Leave the controller clean for the next owner of the mutex.
            self._io.outb(port_status, STATUS_CLEAR_MASK)

    def responding_addresses(self, register=0x05):
        """Return the sensor addresses that answered, in slot order.

        An empty slot, or a module whose maker left the sensor off the board,
        simply does not answer; that is a normal outcome and not an error.
        """
        found = []
        for address in THERMAL_SENSOR_ADDRESSES:
            try:
                self.read_word_bytes(address, register)
            except (OSError, TimeoutError, ValueError):
                continue
            found.append(address)
        return tuple(found)
