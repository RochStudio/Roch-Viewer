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

"""Read-only NCT679x hardware-monitor access.

Companion to :mod:`superio_lpc`, which speaks the other Nuvoton sensor model.
The two are not variants of one protocol; they are different register files
reached different ways, and the distinction is not cosmetic:

  NCT668x (superio_lpc)   an embedded-controller window at the LDN 0x0B base,
                          addressed page/index/data at base+4/5/6, sensors in
                          flat 16-bit blocks at 0x100 and 0x120.

  NCT679x (this module)   a bank-switched register file at the same base,
                          addressed index/data at base+5/6 after selecting a
                          bank through register 0x4E, sensors as single bytes
                          scattered across banks 0-7.

Addressing one with the other's protocol does not fault. It returns 0xFFFF and
repeated bytes, and 0xFFFF passes a temperature band as -0.004 C, which is how
an ASUS ROG MAXIMUS Z790 APEX came to report a 3.7 C CPU. See EC_WINDOW_CHIPS
in superio_lpc for the gate that stops that.

SAFETY -- this module reads sensors and nothing else, on the same terms as
superio_lpc:

  * The only writes are the ones the read path requires: the vendor unlock
    sequence, the logical-device selector, and the bank/index registers that
    address a sensor byte. Every one of those selects *what to read*.
  * There is no write to any sensor, fan, PWM or configuration value, and no
    primitive that could perform one. This chip also drives fan control, so
    that capability is deliberately absent rather than merely unused.
  * Config mode is always exited, including on error.
  * Access is serialised on the named mutex LibreHardwareMonitor and HWiNFO
    take for ISA/LPC access, so a concurrent monitor cannot land between the
    bank select and the read.
"""

from __future__ import annotations

import threading

from lowlevel_io import InpOutByteIO, NamedMutex
from superio_lpc import (
    CONFIG_PORTS,
    ISA_MUTEX_NAME,
    LDN_EC_SPACE,
    NUVOTON_LOCK_BYTE,
    NUVOTON_UNLOCK_BYTE,
    REG_BASE_ADDRESS_HIGH,
    REG_BASE_ADDRESS_LOW,
    REG_CHIP_ID_HIGH,
    REG_CHIP_ID_LOW,
    REG_LOGICAL_DEVICE,
)

# Offsets inside the hardware-monitor window.
HWM_INDEX_OFFSET = 0x05
HWM_DATA_OFFSET = 0x06

# Writing this register on the index port selects which bank the next index
# refers to. A sensor address here is (bank << 8) | register.
BANK_SELECT_REGISTER = 0x4E

# Chip IDs using this register model.
#
# 0xD42B is what the ASUS ROG MAXIMUS Z790 APEX answers on config port 0x2E.
# HWiNFO names that same chip "Nuvoton NCT6798D" in its sensor tree, which is
# where the name here comes from; nothing in this project decodes a model name
# out of the ID itself.
NCT679X_CHIP_IDS = {
    0xD42B: "NCT6798D",
}

# Voltage LSB at the ADC. Every channel in the 0x480 block is a byte, but the
# volts each count is worth depends on the divider in front of that channel,
# which is the board vendor's choice: 8 mV undivided, 16 mV behind a 2:1, and
# 18 mV on the memory-controller channel. So a rail records its own step
# rather than inheriting this one, and this value is only the starting point a
# step is derived from.
VOLTAGE_STEP_VOLTS = 0.008


class Nct679xUnavailable(RuntimeError):
    """Raised when no NCT679x responds, or before one has been detected."""


class Nct679xReader:
    """Detect an NCT679x and read its sensor registers. Read-only."""

    def __init__(self, io=None, mutex=None):
        self._io = io if io is not None else InpOutByteIO()
        self._mutex = mutex if mutex is not None else NamedMutex(ISA_MUTEX_NAME)
        self._lock = threading.Lock()
        self.config_port = None
        self.chip_id = None
        self.chip_name = None
        self.hwm_base = None
        self.last_error = ""

    # -- configuration space (caller holds the mutex) ----------------------

    def _enter_config(self, port):
        self._io.outb(port, NUVOTON_UNLOCK_BYTE)
        self._io.outb(port, NUVOTON_UNLOCK_BYTE)

    def _exit_config(self, port):
        self._io.outb(port, NUVOTON_LOCK_BYTE)

    def _read_config(self, port, register):
        self._io.outb(port, int(register) & 0xFF)
        return self._io.inb(port + 1) & 0xFF

    def detect(self):
        """Find an NCT679x and its monitor window. ``last_error`` explains False."""
        self.last_error = ""
        self.hwm_base = None
        self.chip_id = None
        self.chip_name = None
        declined = []
        if not self._io.is_driver_open():
            self.last_error = "InpOut driver is not open"
            return False
        with self._lock, self._mutex:
            for port in CONFIG_PORTS:
                try:
                    self._enter_config(port)
                    try:
                        high = self._read_config(port, REG_CHIP_ID_HIGH)
                        low = self._read_config(port, REG_CHIP_ID_LOW)
                        chip_id = (high << 8) | low
                        if chip_id in (0x0000, 0xFFFF):
                            continue
                        self._select_logical_device(port, LDN_EC_SPACE)
                        base_high = self._read_config(port, REG_BASE_ADDRESS_HIGH)
                        base_low = self._read_config(port, REG_BASE_ADDRESS_LOW)
                        base = ((base_high << 8) | base_low) & 0xFFFF
                    finally:
                        # Never leave the chip unlocked, even on error.
                        self._exit_config(port)
                except OSError:
                    continue
                if base in (0x0000, 0xFFFF):
                    continue
                name = NCT679X_CHIP_IDS.get(chip_id)
                if name is None:
                    declined.append((port, chip_id))
                    continue
                self.config_port = port
                self.chip_id = chip_id
                self.chip_name = name
                self.hwm_base = base
                return True
        if declined:
            self.last_error = "no NCT679x; declined: " + ", ".join(
                "0x%04X at 0x%02X" % (chip_id, port) for port, chip_id in declined
            )
        else:
            self.last_error = "No Super I/O responded on 0x2E or 0x4E"
        return False

    def _select_logical_device(self, port, ldn):
        self._io.outb(port, REG_LOGICAL_DEVICE)
        self._io.outb(port + 1, int(ldn) & 0xFF)

    # -- sensor registers ---------------------------------------------------

    def _read_locked(self, address):
        """Select the bank, then the register, then read the data port."""
        base = self.hwm_base
        self._io.outb(base + HWM_INDEX_OFFSET, BANK_SELECT_REGISTER)
        self._io.outb(base + HWM_DATA_OFFSET, (int(address) >> 8) & 0xFF)
        self._io.outb(base + HWM_INDEX_OFFSET, int(address) & 0xFF)
        return self._io.inb(base + HWM_DATA_OFFSET) & 0xFF

    def read_byte(self, address):
        """Read one sensor byte addressed as ``(bank << 8) | register``."""
        if self.hwm_base is None:
            raise Nct679xUnavailable("NCT679x has not been detected")
        with self._lock, self._mutex:
            return self._read_locked(address)

    def read_bytes(self, addresses):
        """Read several sensor bytes under one acquisition of the mutex."""
        if self.hwm_base is None:
            raise Nct679xUnavailable("NCT679x has not been detected")
        with self._lock, self._mutex:
            return [self._read_locked(address) for address in addresses]


def decode_temperature(raw):
    """Decode a temperature byte into degrees Celsius.

    Whole degrees, signed: this chip reports the fractional half-degree in a
    separate register the displays here do not use, and every reading was
    confirmed against HWiNFO's own whole-degree presentation of the same chip.
    """
    raw = int(raw) & 0xFF
    return raw - 256 if raw > 127 else raw


def decode_volts(raw, step=VOLTAGE_STEP_VOLTS):
    """Decode a voltage byte at the per-channel volts-per-count."""
    return (int(raw) & 0xFF) * float(step)
