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

"""Read-only ACPI embedded-controller access, for board sensors nothing else
reports.

Fifth and last transport in this project, and the only one that shares its
device with the operating system. The ACPI EC is driven by Windows' own ACPI
driver; this module reads bytes out of its RAM alongside it, which is what
LibreHardwareMonitor and HWiNFO do and why they take the same mutex.

It exists because a handful of readings live nowhere else. On the Z790 bench
HWiNFO's "ASUS EC" section carries the VRM temperature, and the Super I/O this
project already reads carries no VRM channel at all -- the row was blank not
because the board lacks the sensor but because it is on this controller.

SAFETY - this module reads and nothing else:

  * The EC has two commands that matter: RD_EC (0x80) and WR_EC (0x81). Only
    RD_EC is defined here. There is no write-command constant, no data-write
    primitive, and no code path that could assemble one, so no EC register can
    be modified through this module. The capability is absent rather than
    merely unused, which matters more here than anywhere else in this project:
    on many boards the EC owns fan control and charging.
  * The writes that do happen are the read protocol itself -- the RD_EC
    command byte and the register address. Both select *what to read*, exactly
    as writing an index into an SMBus HOST_COMMAND does.
  * Every wait is bounded. A controller that stops answering makes this module
    give up and report nothing; it can never spin holding the EC.
  * Access is serialised on the named mutex the mainstream monitoring tools
    take for the embedded controller, so a read cannot land between another
    tool's command byte and its data byte.
  * The status register is checked before each step rather than assumed, so a
    transaction left half-finished by anyone else is detected rather than
    completed with someone else's address.
"""

from __future__ import annotations

import threading
import time

# The mutex mainstream monitoring tools take before driving the EC.
EC_MUTEX_NAME = "Global\\Access_EC"

# Standard ACPI embedded-controller ports.
EC_DATA_PORT = 0x62
EC_COMMAND_PORT = 0x66

# Status register bits, read from the command port.
STATUS_OUTPUT_FULL = 1 << 0     # OBF: a byte is waiting to be read
STATUS_INPUT_FULL = 1 << 1      # IBF: the controller has not taken ours yet

# The read command. Its counterpart WR_EC (0x81) is deliberately absent; see
# the safety note above.
COMMAND_READ = 0x80

# Bounded waits. The EC is slow by design and answers in microseconds when
# idle, but it can be busy with the OS driver, so the budget is generous and
# the polling interval short.
STATUS_TIMEOUT_S = 0.02
STATUS_POLL_S = 0.0001


class EcUnavailable(RuntimeError):
    """Raised when the embedded controller cannot be driven."""


class AcpiEcReader:
    """Serialised, read-only ACPI EC byte reader."""

    def __init__(self, io=None, mutex=None, monotonic=time.monotonic,
                 sleep=time.sleep, timeout=STATUS_TIMEOUT_S):
        if io is None:
            from lowlevel_io import InpOutByteIO

            io = InpOutByteIO()
        self._io = io
        if mutex is None:
            from lowlevel_io import NamedMutex

            mutex = NamedMutex(EC_MUTEX_NAME)
        self._mutex = mutex
        self._lock = threading.Lock()
        self._monotonic = monotonic
        self._sleep = sleep
        timeout = float(timeout)
        if not 0.0 < timeout <= 0.25:
            raise ValueError("EC timeout must be within (0, 0.25]")
        self._timeout = timeout
        self.last_error = ""

    def is_driver_open(self):
        return self._io.is_driver_open()

    def _wait_for(self, bit, is_set):
        """Poll the status register until one bit reaches a state."""
        deadline = self._monotonic() + self._timeout
        while True:
            status = self._io.inb(EC_COMMAND_PORT) & 0xFF
            if bool(status & bit) == is_set:
                return status
            if self._monotonic() > deadline:
                raise TimeoutError(
                    "EC status 0x%02X never reached %s on bit 0x%02X"
                    % (status, "set" if is_set else "clear", bit)
                )
            self._sleep(STATUS_POLL_S)

    def _drain(self):
        """Discard a byte another owner left in the output buffer.

        Starting a read with OBF already set would return that byte as though
        it were ours.
        """
        status = self._io.inb(EC_COMMAND_PORT) & 0xFF
        if status & STATUS_OUTPUT_FULL:
            self._io.inb(EC_DATA_PORT)

    def _read_locked(self, register):
        self._drain()
        self._wait_for(STATUS_INPUT_FULL, False)
        self._io.outb(EC_COMMAND_PORT, COMMAND_READ)
        self._wait_for(STATUS_INPUT_FULL, False)
        self._io.outb(EC_DATA_PORT, int(register) & 0xFF)
        self._wait_for(STATUS_OUTPUT_FULL, True)
        return self._io.inb(EC_DATA_PORT) & 0xFF

    def read_byte(self, register):
        """Read one byte of EC RAM. Raises on timeout; never returns partial."""
        if not self._io.is_driver_open():
            raise EcUnavailable("InpOut driver is not open")
        with self._lock, self._mutex:
            return self._read_locked(register)

    def read_bytes(self, registers):
        """Read several EC registers under one acquisition of the mutex."""
        if not self._io.is_driver_open():
            raise EcUnavailable("InpOut driver is not open")
        with self._lock, self._mutex:
            return [self._read_locked(register) for register in registers]


def decode_temperature(raw):
    """Decode an EC temperature byte into degrees Celsius.

    Whole signed degrees, the same encoding the Super I/O uses.
    """
    raw = int(raw) & 0xFF
    return raw - 256 if raw > 127 else raw


# --- Board identity.
#
# The EC protocol above is the ACPI standard and works on any board. What is
# in EC RAM is not: the register map is the board vendor's, so reading 0x33 on
# a machine from another vendor returns whatever that vendor put there, and a
# byte between 20 and 70 would pass the band below and be printed as a VRM
# temperature. That is exactly the failure this project already hit once, when
# an MSI Super I/O map was applied to an ASUS chip and produced a 3.7 C CPU.
#
# So the map is gated on the vendor rather than trusted everywhere. The entry
# below was confirmed on one ASUS board; other ASUS boards are believed to
# share this layout, which is why the gate is the vendor and not the model,
# and that belief is the weakest link here rather than the decode.
BOARD_VENDOR_PREFIXES = ("ASUS",)


def is_asus_board(board_name):
    """Whether this board's vendor uses the register map below."""
    name = str(board_name or "").strip().upper()
    return any(prefix in name for prefix in BOARD_VENDOR_PREFIXES)


# --- Board sensors.
#
# key -> (EC register, min C, max C).
#
# Confirmed on ASUS ROG MAXIMUS Z790 APEX against HWiNFO's own "ASUS EC"
# section, and then through an idle -> all-core load -> idle cycle, because a
# byte that happens to read 40 proves nothing on its own:
#
#   VRM  0x33  40 C idle, 43 C under an all-core load, 40 C recovered.
#              HWiNFO's ASUS EC VRM read 40 C with a 44 C maximum over its own
#              window: the same value at rest and the same ceiling loaded.
#
# The size of the response is what identifies it, not just the value. Four
# other registers -- 0x30, 0x42, 0x53 and 0x59 -- swing 33 C to 65 C and back
# across the same cycle, which is a die sensor mirrored four times. A VRM
# carries far more thermal mass than the die and warms a few degrees where the
# die swings thirty, so a channel sitting at 40 C and moving 3 C is the one
# that is measuring the regulator.
CONFIRMED_EC_TEMPERATURES = {
    "vrm": (0x33, -20.0, 125.0),
}

# The detected reader, kept so a refusal is not re-tried on every refresh.
_READER = []


def validate_temperature(key, celsius):
    """Return the reading, or None when it cannot be that sensor."""
    sensor = CONFIRMED_EC_TEMPERATURES.get(key)
    if sensor is None:
        return None
    _register, minimum, maximum = sensor
    try:
        celsius = float(celsius)
    except (TypeError, ValueError):
        return None
    return celsius if minimum <= celsius <= maximum else None


def _detected_reader(reader_factory):
    """Return a working reader, or None. Resolved once."""
    if _READER:
        return _READER[0] or None
    try:
        reader = reader_factory()
        if not reader.is_driver_open():
            _READER.append(False)
            return None
        # Prove the controller answers before it is trusted, so a board whose
        # EC does not respond costs one probe rather than one per refresh.
        reader.read_byte(next(iter(CONFIRMED_EC_TEMPERATURES.values()))[0])
    except Exception:
        _READER.append(False)
        return None
    _READER.append(reader)
    return reader


def read_temperatures(reader_factory=None, sensors=None):
    """Return ``{sensor key: celsius}`` for the confirmed EC sensors."""
    sensors = CONFIRMED_EC_TEMPERATURES if sensors is None else sensors
    if not sensors:
        return {}
    if reader_factory is None:
        reader_factory = AcpiEcReader
    reader = _detected_reader(reader_factory)
    if reader is None:
        return {}
    values = {}
    try:
        for key, (register, _minimum, _maximum) in sensors.items():
            try:
                celsius = decode_temperature(reader.read_byte(register))
            except Exception:
                continue
            checked = validate_temperature(key, celsius)
            if checked is not None:
                values[key] = checked
    except Exception:
        return {}
    return values


def temperature_text(key, reader_factory=None):
    """Format one EC sensor for a table row, or None when unavailable."""
    celsius = read_temperatures(reader_factory=reader_factory).get(key)
    return None if celsius is None else f"{celsius:.1f} °C"
