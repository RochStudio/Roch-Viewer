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

"""Read-only Super I/O (LPC) sensor access.

Third and last transport in this project.  Some rails live nowhere else:

  * SMU PM table  -> VDDCR_SOC, VDDG, CLDO, CDD_MISC   (amd_smu_voltages)
  * DDR5 PMIC     -> DRAM VDD/VDDQ/VPP                 (ddr5_pmic)
  * Super I/O     -> CPU VDDIO, Vcore, VTT             (this module)

CPU VDDIO is provably absent from the first two: raising it 1.44 -> 1.47 V in
BIOS moved nothing in the PM table, and the DDR5 PMIC exposes only SWA/SWB/SWC,
VIN_Bulk and its two VOUT rails.  HWiNFO reports VDDIO, Vcore and VTT in its
motherboard section, which is this chip.

SAFETY — this module reads sensors and nothing else:

  * The only writes are the ones the read path requires: the vendor unlock
    sequence, the logical-device selector used to find the sensor window, and
    the page/index registers that address a sensor byte.  Every one of those
    selects *what to read*.
  * There is no write to any sensor, fan, PWM or configuration value, and no
    primitive that could perform one.  The Super I/O also drives fan control,
    so that capability is deliberately absent rather than merely unused.
  * Config mode is always exited, including on error, mirroring the SMN
    selector-restore discipline used elsewhere in this project.
  * Access is serialised on the named mutex LibreHardwareMonitor and HWiNFO
    take for ISA/LPC access.
"""

from __future__ import annotations

import threading

from rochviewer.hardware.lowlevel_io import InpOutByteIO, NamedMutex

# Believable ranges live with the rail definitions, so the transport and the
# view can never disagree about what counts as a plausible reading.
from rochviewer.sensors.voltage_rails import RAILS_BY_KEY, validate_voltage

# The mutex mainstream monitoring tools take before driving LPC.
ISA_MUTEX_NAME = "Global\\Access_ISABUS.HTP.Method"

# Super I/O configuration windows. Boards use one or the other.
CONFIG_PORTS = (0x2E, 0x4E)

# Nuvoton unlock/lock sequence.
NUVOTON_UNLOCK_BYTE = 0x87
NUVOTON_LOCK_BYTE = 0xAA

# Configuration registers.
REG_LOGICAL_DEVICE = 0x07
REG_CHIP_ID_HIGH = 0x20
REG_CHIP_ID_LOW = 0x21
REG_BASE_ADDRESS_HIGH = 0x60
REG_BASE_ADDRESS_LOW = 0x61

# Logical device carrying the embedded-controller sensor window.
LDN_EC_SPACE = 0x0B

# Offsets inside the EC window.
EC_PAGE_OFFSET = 0x04
EC_INDEX_OFFSET = 0x05
EC_DATA_OFFSET = 0x06

# The page register needs this preamble before the bank number; without it
# every read returns the same byte.
EC_PAGE_SELECT = 0xFF

# Known chip IDs. The high byte identifies the family; the low nibble is a
# revision, so compare on the masked value.
CHIP_IDS = {
    0xD592: "NCT6687D",
    0xD591: "NCT6687D",
    0xC732: "NCT6683D",
    0xD452: "NCT6686D",
    0xC562: "NCT6791D",
    0xC803: "NCT6796D",
    0xD121: "NCT6798D",
    # Named by HWiNFO reading this same chip on an ASUS ROG MAXIMUS Z790 APEX.
    # Listed so the refusal below can name what it declined; the sensors are
    # read by nct679x, not from here.
    0xD42B: "NCT6798D",
}

# Chips whose sensors the page/index/data window below actually addresses.
#
# Being named in CHIP_IDS is not the same as being readable here. The window
# this reader drives -- EC page/index/data at ec_base+4/5/6, sensors in the
# 0x100 and 0x120 blocks -- is the NCT668x embedded-controller model. The
# NCT679x parts expose their sensors through a different, bank-switched
# register model, and they do not fault when addressed this way: they answer
# 0xFFFF and repeated bytes.
#
# That silence is why this gate has to exist rather than being left to the
# per-reading band checks. A voltage band rejects 0xFFFF, so the rails simply
# went blank. A temperature band does not: 0xFFFF decodes to -0.004 C and
# 0x03BC to 3.734 C, both inside every sensible band, so an ASUS ROG MAXIMUS
# Z790 APEX (chip 0xD42B at config port 0x2E) reported a 3.7 C CPU and a
# -0.0 C PCH as though they were measurements.
EC_WINDOW_CHIPS = frozenset({"NCT6683D", "NCT6686D", "NCT6687D"})

# Voltage sensors on NCT668x live in a contiguous 16-bit block.
VOLTAGE_BLOCK_START = 0x120
VOLTAGE_BLOCK_COUNT = 16

# Sensor LSB. The low 5 bits of every word read zero, and a BIOS change of
# exactly -60 mV moved the VDDIO channel by exactly -480 raw counts, giving
# 0.125 mV per count - a clean 1/8 mV step.
SENSOR_STEP_VOLTS = 0.000125

# Several channels sit behind a 2:1 divider, so they decode at half the step.
SENSOR_STEP_DIVIDED = SENSOR_STEP_VOLTS / 2.0

# rail key -> (sensor address, volts per count, min volts, max volts).
#
# Confirmed on MSI B850MPOWER (NCT6687D, config port 0x4E, window 0x0A20).
# Each entry was established against a known-good reading, not inferred:
#
#   CPU VDDIO  0x0126  changed in BIOS 1.47 -> 1.41 V, moving the channel
#                      0x2E60 -> 0x2C80, i.e. -480 counts for -60 mV, giving
#                      exactly 0.125 mV/count with no divider. Nothing else in
#                      the window moved with a clean scale in the right
#                      direction. Reads ~14 mV above setpoint at both points,
#                      matching HWiNFO's own offset.
#
#   VTT        0x0136  ranged 4.0880-4.0920 across an idle/load cycle, which
#                      halves to 2.0440-2.0460 -- HWiNFO's TM5 min AND max of
#                      2.044 / 2.046 exactly. Two matched endpoints fix both
#                      the scale and the absence of an offset.
#
#   Vcore      0x0128  the only load-tracking channel: 2.5520 -> 2.3240 under
#                      an all-core load. Halved, 1.1620-1.2760 V sits inside
#                      HWiNFO's TM5 range of 1.036-1.326 V, and the divider is
#                      the same one VTT and CPU NB/SoC independently confirm.
#
# The divider was not guessed: 0x0124 halves to 1.2060 V against HWiNFO's CPU
# NB/SoC 1.206 V exactly, a second fixed rail agreeing with VTT.
#
# NOTE: this mapping is BOARD-SPECIFIC. The index-to-rail assignment and the
# per-channel dividers are chosen by the board vendor, so they must be
# re-derived per model rather than assumed to carry over.
# NOTE: the core rail is deliberately absent. 0x0128 is the VRM-side Vcore
# measurement, but VDDCR_VDD is served from the CPU's own SVI3 telemetry in the
# PM table instead, matching HWiNFO's "(SVI3 TFN)" reading. The two measure the
# same rail in different places and differ by tens of millivolts.
CONFIRMED_SENSORS = {
    "vddio_mem": (0x126, SENSOR_STEP_VOLTS),
    "vtt": (0x136, SENSOR_STEP_DIVIDED),
}

# Believable band for any board voltage sensor, used to shortlist candidates.
CANDIDATE_MIN_VOLTS = 0.20
CANDIDATE_MAX_VOLTS = 6.00


class SuperIoUnavailable(RuntimeError):
    """Raised when no supported Super I/O responds."""


class SuperIoReader:
    """Detect the Super I/O and read its sensor window. Read-only."""

    def __init__(self, io=None, mutex=None):
        self._io = io if io is not None else InpOutByteIO()
        self._mutex = mutex if mutex is not None else NamedMutex(ISA_MUTEX_NAME)
        self._lock = threading.Lock()
        self.config_port = None
        self.chip_id = None
        self.chip_name = None
        self.ec_base = None
        self.last_error = ""

    # -- configuration-space helpers (caller holds the mutex) --------------

    def _enter_config(self, port):
        self._io.outb(port, NUVOTON_UNLOCK_BYTE)
        self._io.outb(port, NUVOTON_UNLOCK_BYTE)

    def _exit_config(self, port):
        self._io.outb(port, NUVOTON_LOCK_BYTE)

    def _read_config(self, port, register):
        self._io.outb(port, int(register) & 0xFF)
        return self._io.inb(port + 1) & 0xFF

    def _select_logical_device(self, port, ldn):
        self._io.outb(port, REG_LOGICAL_DEVICE)
        self._io.outb(port + 1, int(ldn) & 0xFF)

    def detect(self):
        """Find a supported Super I/O and its sensor window.

        Returns True on success; ``last_error`` explains a False.
        """
        self.last_error = ""
        # Cleared up front so a second call cannot leave a window from a
        # previous detection addressable after this one declines the chip.
        self.ec_base = None
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
                        base_high = self._read_config(
                            port, REG_BASE_ADDRESS_HIGH
                        )
                        base_low = self._read_config(port, REG_BASE_ADDRESS_LOW)
                        base = ((base_high << 8) | base_low) & 0xFFFF
                    finally:
                        # Never leave the chip unlocked, even on error.
                        self._exit_config(port)
                except OSError:
                    continue
                if base in (0x0000, 0xFFFF):
                    continue
                name = CHIP_IDS.get(
                    chip_id, CHIP_IDS.get(chip_id & 0xFFF0, "unknown")
                )
                if name not in EC_WINDOW_CHIPS:
                    # Keep the identity so the refusal can be diagnosed, but
                    # leave ec_base unset: read_word must not address a window
                    # this chip does not have. Carry on to the other port in
                    # case the board answers on both.
                    #
                    # Recorded, not overwritten. An unpopulated config port can
                    # answer with junk that is neither 0x0000 nor 0xFFFF, so
                    # keeping the last decline would let that junk displace the
                    # real chip: this board reports 0xD42B on 0x2E and 0x9090
                    # on 0x4E, and 0x9090 is the one nobody can act on.
                    declined.append((port, chip_id, name))
                    if self.chip_id is None:
                        self.config_port = port
                        self.chip_id = chip_id
                        self.chip_name = name
                    continue
                self.config_port = port
                self.chip_id = chip_id
                self.chip_name = name
                self.ec_base = base
                return True
        if declined:
            self.last_error = (
                "no Super I/O exposing the NCT668x sensor window; declined: "
                + ", ".join(
                    "0x%04X (%s) at 0x%02X" % (chip_id, name, port)
                    for port, chip_id, name in declined
                )
            )
        else:
            self.last_error = "No supported Super I/O responded on 0x2E or 0x4E"
        return False

    # -- sensor window ------------------------------------------------------

    def _read_ec_byte_locked(self, address):
        """Address and read one sensor byte.

        The data port does not auto-increment on this chip, so every byte is
        addressed individually. The 0xFF preamble on the page register is
        required; without it every read returns the same byte.
        """
        base = self.ec_base
        self._io.outb(base + EC_PAGE_OFFSET, EC_PAGE_SELECT)
        self._io.outb(base + EC_PAGE_OFFSET, (int(address) >> 8) & 0xFF)
        self._io.outb(base + EC_INDEX_OFFSET, int(address) & 0xFF)
        return self._io.inb(base + EC_DATA_OFFSET) & 0xFF

    def read_bytes(self, address, count):
        if self.ec_base is None:
            raise SuperIoUnavailable("Super I/O has not been detected")
        with self._lock, self._mutex:
            return [
                self._read_ec_byte_locked(int(address) + offset)
                for offset in range(int(count))
            ]

    def read_word(self, address):
        """Read a big-endian 16-bit sensor value."""
        high, low = self.read_bytes(address, 2)
        return ((high << 8) | low) & 0xFFFF

    def read_voltage_block(self, start=VOLTAGE_BLOCK_START,
                           count=VOLTAGE_BLOCK_COUNT):
        """Return ``{address: raw word}`` for the voltage sensor block."""
        values = {}
        for index in range(int(count)):
            address = int(start) + index * 2
            try:
                values[address] = self.read_word(address)
            except (OSError, SuperIoUnavailable):
                continue
        return values


# Detected readers, keyed by factory, so repeat reads skip chip detection.
_DETECTED = {}


def decode_sensor_volts(raw, step=SENSOR_STEP_VOLTS):
    """Decode a raw sensor word into volts."""
    return (int(raw) & 0xFFFF) * float(step)


def decode_millivolts(raw):
    """Legacy helper used by the research probe's raw listing."""
    return (int(raw) & 0xFFFF) / 1000.0


def read_board_rails(reader_factory=SuperIoReader, sensors=None):
    """Read the confirmed Super I/O rails. Returns ``{rail key: volts}``.

    Anything decoding outside its believable range is dropped rather than
    displayed, keeping this transport fail-closed like the other two.
    """
    sensors = CONFIRMED_SENSORS if sensors is None else sensors
    if not sensors:
        return {}
    try:
        reader = _DETECTED.get(reader_factory)
        if reader is None:
            reader = reader_factory()
            if not reader.detect():
                return {}
            # Detection unlocks and re-locks the configuration window; the
            # chip cannot move while the machine is running, so keep it.
            _DETECTED[reader_factory] = reader
        values = {}
        for key, (address, step) in sensors.items():
            rail = RAILS_BY_KEY.get(key)
            if rail is None:
                continue
            try:
                volts = decode_sensor_volts(reader.read_word(address), step)
                values[key] = validate_voltage(rail, volts)
            except (OSError, SuperIoUnavailable, ValueError):
                continue
        return values
    except Exception:
        return {}


def is_candidate_voltage(volts):
    """True when a decoded sensor could plausibly be a board rail."""
    try:
        volts = float(volts)
    except (TypeError, ValueError):
        return False
    return CANDIDATE_MIN_VOLTS <= volts <= CANDIDATE_MAX_VOLTS
