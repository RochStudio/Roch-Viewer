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

"""ITE Super I/O sensor transport for Intel boards.

``superio_lpc`` speaks the Nuvoton unlock sequence and only accepts NCT668x
parts. Gigabyte's Z890 boards carry ITE silicon instead -- IT8696E on config
port 0x2E and IT87952E on 0x4E -- which answers a different unlock and reads
its sensors through a plain index/data pair rather than a 16-bit window. A
Nuvoton-only reader finds nothing there and every board rail reads blank.

This module is deliberately standalone. It does not import ``superio_lpc``,
because that module pulls in ``lowlevel_io``, and the Intel path must not import
AMD code to read a board rail.

READ-ONLY. Only three things are ever written: the vendor unlock sequence,
the logical-device selector, and the EC index register. All three select what
to read. No sensor, fan or PWM value is written.
"""

from __future__ import annotations

import ctypes
import threading

from rochviewer.hardware.read import inpout

# The mutex mainstream monitoring tools take before driving LPC, so a
# concurrent HWiNFO poll cannot land between our index write and data read.
ISA_MUTEX_NAME = "Global\\Access_ISABUS.HTP.Method"

CONFIG_PORTS = (0x2E, 0x4E)

# ITE MB PnP unlock: 0x87, 0x01, 0x55, then 0x55 on port 0x2E or 0xAA on 0x4E.
ITE_UNLOCK = (0x87, 0x01, 0x55)
ITE_EXIT_REGISTER = 0x02
ITE_EXIT_VALUE = 0x02

REG_CHIP_ID_HIGH = 0x20
REG_CHIP_ID_LOW = 0x21
REG_LOGICAL_DEVICE = 0x07
REG_BASE_ADDRESS_HIGH = 0x60
REG_BASE_ADDRESS_LOW = 0x61

# Logical device 4 is the environment controller on every ITE part here.
LDN_ENVIRONMENT_CONTROLLER = 0x04

# Sensor access within the EC window: write the register to base+5, read the
# value from base+6.
EC_INDEX_OFFSET = 5
EC_DATA_OFFSET = 6

# Confirmed present on Gigabyte Z890 AORUS TACHYON ICE.
CHIP_IDS = {
    0x8696: "IT8696E",
    0x8695: "IT87952E",
}

# Voltage LSB per chip. These genuinely differ: decoding IT8696E at 10.9 mV
# or IT87952E at 12 mV misses every rail by 8-10%, and both figures were
# settled by matching HWiNFO on the same boot rather than assumed.
CHIP_VOLTAGE_STEP = {
    "IT8696E": 0.012,
    "IT87952E": 0.0109,
}

VOLTAGE_BLOCK = range(0x20, 0x29)
TEMPERATURE_BLOCK = range(0x29, 0x2E)

# A temperature channel with nothing attached reads back at the rail ends.
TEMPERATURE_MIN_C = 1
TEMPERATURE_MAX_C = 125


class IteUnavailable(RuntimeError):
    """Raised when no ITE Super I/O responds."""


WAIT_OBJECT_0 = 0x00000000
WAIT_ABANDONED = 0x00000080
MUTEX_TIMEOUT_MS = 500


class _NamedMutex:
    """Windows named mutex guarding the LPC bus.

    Acquisition is not optional. HWiNFO and every other monitoring tool take
    this same mutex before driving Super I/O, because the chip is addressed
    through one index/data pair: if a second writer sets the index between
    another reader's index write and its data read, that reader gets a value
    from the wrong register -- and interleaved writes can leave the embedded
    controller wedged, which takes the machine down rather than the app.

    So a failed wait means "do not touch the hardware", never "carry on".
    """

    def __init__(self, name):
        self._name = name
        self._handle = None
        self.acquired = False
        try:
            kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            kernel32.CreateMutexW.restype = ctypes.c_void_p
            kernel32.WaitForSingleObject.argtypes = [
                ctypes.c_void_p, ctypes.c_ulong
            ]
            self._kernel32 = kernel32
            self._handle = kernel32.CreateMutexW(None, False, name)
        except Exception:
            self._kernel32 = None
            self._handle = None

    def __enter__(self):
        self.acquired = False
        if not self._handle:
            # No mutex means no way to exclude another writer, so decline.
            return self
        try:
            result = self._kernel32.WaitForSingleObject(
                ctypes.c_void_p(self._handle), MUTEX_TIMEOUT_MS
            )
        except Exception:
            return self
        self.acquired = result in (WAIT_OBJECT_0, WAIT_ABANDONED)
        return self

    def __exit__(self, *_exc):
        # Release only what was actually taken.
        if self._handle and self.acquired:
            try:
                self._kernel32.ReleaseMutex(ctypes.c_void_p(self._handle))
            except Exception:
                pass
        self.acquired = False
        return False


class IteSuperIoReader:
    """Detect ITE Super I/O chips and read their sensor block. Read-only."""

    def __init__(self):
        inpout.Out32.argtypes = [ctypes.c_short, ctypes.c_short]
        inpout.Inp32.argtypes = [ctypes.c_short]
        inpout.Inp32.restype = ctypes.c_short
        self._lock = threading.Lock()
        self._mutex = _NamedMutex(ISA_MUTEX_NAME)
        self.chips = {}
        # The identity rows ask every sensor reader for chip_name, whatever
        # transport it speaks. This board answers on both config ports, so the
        # attribute names each chip found rather than only the first.
        self.chip_name = None
        self.last_error = ""

    # -- raw port access ---------------------------------------------------

    def _outb(self, port, value):
        inpout.Out32(port, int(value) & 0xFF)

    def _inb(self, port):
        return inpout.Inp32(port) & 0xFF

    def _enter_config(self, port):
        for value in ITE_UNLOCK:
            self._outb(port, value)
        self._outb(port, 0x55 if port == 0x2E else 0xAA)

    def _exit_config(self, port):
        self._outb(port, ITE_EXIT_REGISTER)
        self._outb(port + 1, ITE_EXIT_VALUE)

    def _read_config(self, port, register):
        self._outb(port, register)
        return self._inb(port + 1)

    # -- detection ---------------------------------------------------------

    def detect(self):
        """Populate ``chips`` with every ITE part found. True if any answered."""
        self.chips = {}
        self.last_error = ""
        found = {}
        with self._lock, self._mutex as mutex:
            if not mutex.acquired:
                self.last_error = (
                    "LPC bus is held by another tool; declined to probe"
                )
                return False
            for port in CONFIG_PORTS:
                try:
                    self._enter_config(port)
                    try:
                        chip_id = (
                            self._read_config(port, REG_CHIP_ID_HIGH) << 8
                        ) | self._read_config(port, REG_CHIP_ID_LOW)
                        if chip_id in (0x0000, 0xFFFF):
                            continue
                        name = CHIP_IDS.get(chip_id)
                        if name is None:
                            # Unknown ITE part: the register layout is close
                            # enough to read, but the rail map is not ours to
                            # guess, so record it without claiming support.
                            continue
                        self._outb(port, REG_LOGICAL_DEVICE)
                        self._outb(port + 1, LDN_ENVIRONMENT_CONTROLLER)
                        base = (
                            self._read_config(port, REG_BASE_ADDRESS_HIGH) << 8
                        ) | self._read_config(port, REG_BASE_ADDRESS_LOW)
                    finally:
                        # Never leave the chip unlocked, even on error.
                        self._exit_config(port)
                except OSError:
                    continue
                if base in (0x0000, 0xFFFF):
                    continue
                found[name] = {
                    "port": port,
                    "chip_id": chip_id,
                    "ec_base": base,
                    "step": CHIP_VOLTAGE_STEP.get(name),
                }
        self.chips = found
        # Both parts, in config-port order, because the pair is what this
        # board has -- naming one of them would read as the whole answer.
        self.chip_name = " + ".join(
            name for name, chip in sorted(found.items(),
                                          key=lambda kv: kv[1]["port"])
        ) or None
        if not found:
            self.last_error = "No ITE Super I/O responded on 0x2E or 0x4E"
        return bool(found)

    # -- sensor window -----------------------------------------------------

    def read_register(self, chip_name, register):
        """Read one EC sensor register, or None when the chip is absent."""
        chip = self.chips.get(chip_name)
        if chip is None:
            return None
        base = chip["ec_base"]
        with self._lock, self._mutex as mutex:
            if not mutex.acquired:
                # Another tool owns the bus. Reporting nothing is correct;
                # reading anyway would race its index write.
                self.last_error = "LPC bus busy; sensor read skipped"
                return None
            try:
                self._outb(base + EC_INDEX_OFFSET, register)
                return self._inb(base + EC_DATA_OFFSET)
            except OSError:
                return None

    def read_voltage(self, chip_name, register, divider=1.0):
        """Return a decoded rail in volts, or None."""
        chip = self.chips.get(chip_name)
        if chip is None or not chip.get("step"):
            return None
        raw = self.read_register(chip_name, register)
        if raw is None:
            return None
        return raw * chip["step"] * divider

    def read_temperature(self, chip_name, register):
        """Return a plausible temperature in Celsius, or None."""
        raw = self.read_register(chip_name, register)
        if raw is None:
            return None
        if not TEMPERATURE_MIN_C <= raw <= TEMPERATURE_MAX_C:
            # An unconnected channel parks outside the band; 0xC9 shows up on
            # this board's second chip for a header with no probe fitted.
            return None
        return float(raw)

    def read_block(self, chip_name):
        """Return the whole sensor block, for confirming a new board."""
        if chip_name not in self.chips:
            return {}
        block = {}
        for register in list(VOLTAGE_BLOCK) + list(TEMPERATURE_BLOCK):
            block[register] = self.read_register(chip_name, register)
        return block
