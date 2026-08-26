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

"""Board sensor rails for Intel desktop platforms, read over Super I/O.

The memory controller reports VCCSA and VDDQ TX directly through MCHBAR, so
those two need nothing from this module. DRAM voltage and the CPU auxiliary
rail are board measurements instead: nothing in the memory controller carries
them, and they reach the system only through the Super I/O chip's sensor block.

That transport is shared with the AM5 path and is board-agnostic, but the
index-to-rail assignment inside the sensor block is chosen by the board vendor
and does not carry between models. So this module keeps its own map rather than
borrowing the AM5 one, exactly as ``superio_lpc`` warns.

``CONFIRMED_RAILS`` stays empty until an index has been matched against a
known-good reading on the specific board. An unconfirmed index prints a
believable but false voltage, which is worse than printing nothing.
"""

from __future__ import annotations

# Sensor LSB, repeated from superio_lpc rather than imported: that module pulls
# in lowlevel_io, and the Intel path must not import AMD code to read a board rail.
# Several channels sit behind a 2:1 divider and decode at half the step.
SENSOR_STEP_VOLTS = 0.000125
SENSOR_STEP_DIVIDED = SENSOR_STEP_VOLTS / 2.0

# rail key -> (row label, min volts, max volts).
# The bands are what the rail can physically be on a desktop Intel board, used
# only to reject a decode that cannot be right.
INTEL_RAILS = {
    "vdimm": ("DRAM", 0.90, 2.10),
    "cpu_aux": ("CPU AUX", 0.80, 2.50),
    "vcore": ("Vcore", 0.40, 1.80),
    "cpu_sa": ("CPU SA (VRM)", 0.60, 1.70),
    # The memory-controller supply. HWiNFO calls it IMC VDD; ASUS BIOS and the
    # DDR5 tuning world call it VDD2, which is the name a tuner is looking for.
    # Band set around the DDR5 range: 1.10 V is roughly JEDEC, and past 1.60 V
    # the number is a failed decode rather than an aggressive setting.
    "vdd2": ("VDD2", 0.90, 1.60),
    # The termination rail. Nominally around 1.05 V on this socket; past the
    # bounds below the number is a failed decode rather than a setting.
    "vtt": ("VTT", 0.60, 1.60),
    # Always-on VccNN island and the I/O rail. Both are board measurements on
    # LGA 1851 and read straight off the Super I/O.
    "vnnaon": ("CPU VNNAON", 0.40, 1.40),
    "vccio": ("VCCIO", 0.80, 1.80),
}

# rail key -> (sensor address, volts per count).
#
# Confirmed on MSI PRO Z790-P WIFI DDR4 (MS-7E06), NCT6687D at config port
# 0x4E, sensor window 0x0A20, against HWiNFO's reading of this same chip taken
# on the same boot:
#
#   DRAM     index 4  0x0128  full step -> 1.572 V.  HWiNFO "DRAM" 1.572 V,
#                                          exact, and the BIOS is set to 1.57.
#   CPU AUX  index 6  0x012C  full step -> 1.816 V.  HWiNFO "CPU AUX" 1.812 V,
#                                          4 mV apart across two separate
#                                          reads of a live rail.
#
# Neither uses the 2:1 divider. Two further channels corroborate the window
# without being claimed as rails: index 5 halves to 1.292 V against the
# 1.291 V this machine reports for VCCSA out of MCHBAR (HWiNFO "CPU SA"
# 1.288 V), and index 8 halves to 3.336 V, the board's +3.3 V rail.
#
# This mapping is board-specific. It must be re-derived on any other model.
# Vcore and CPU SA were established a second way, because a rail that moves
# cannot be pinned by matching one earlier capture. Loading all sixteen threads
# and re-sampling the block, 0x0124 fell 2.6080 -> 2.5240 (1.306 -> 1.262 V
# halved) and recovered on release, by far the largest movement in the window
# and the droop signature only a core rail has. At idle it halves to 1.306 V
# against HWiNFO's Vcore 1.310.
#
# 0x012A halves to 1.290 V, within 2 mV of HWiNFO's CPU SA 1.288 and within
# 1 mV of the 1.291 V this machine reports for VCCSA out of MCHBAR: two
# independent instruments and a second transport agreeing on one rail.
#
# The same run corroborated the two rails above without being aimed at them:
# under full CPU load 0x0128 (DRAM) and 0x012C (CPU AUX) did not move, which is
# what a memory rail and an auxiliary rail should do.
CONFIRMED_RAILS = {
    "vdimm": (0x128, 0.000125),
    "cpu_aux": (0x12C, 0.000125),
    "vcore": (0x124, 0.0000625),
    "cpu_sa": (0x12A, 0.0000625),
}

# Full capture kept for the record, since it is what the mapping was chosen
# from and what a re-derivation on another board would be compared against.
#
# Captured on MSI PRO Z790-P WIFI DDR4 (MS-7E06), NCT6687D at config port 0x4E,
# sensor window 0x0A20, while the board ran DDR4-4000 18-16-16-28. Decoding the
# nine responding channels at both steps:
#
#   index  addr    raw     full step   halved
#   0      0x0120  0x3EA0    2.004 V   1.002 V
#   1      0x0122  0x3E40    1.992 V   0.996 V
#   2      0x0124  0x51A0    2.612 V   1.306 V
#   3      0x0126  0x0620    0.196 V   0.098 V
#   4      0x0128  0x3120    1.572 V   0.786 V
#   5      0x012A  0x50C0    2.584 V   1.292 V
#   6      0x012C  0x38C0    1.816 V   0.908 V
#   7      0x012E  0x5EC0    3.032 V   1.516 V
#   8      0x0130  0xD080    6.672 V   3.336 V
#
# Indices 9-15 read zero.
#
# Before the HWiNFO comparison, index 4 at the full step (1.572 V) and index 7
# halved (1.516 V) were both believable for this kit, and the raw listing
# printed by superio_probe made a third look right as well: that report decodes
# with a legacy raw/1000 helper rather than the sensor step, which turns
# index 3 into a plausible-looking 1.568 V when the rail is actually 0.196 V.
# The measurement, not the resemblance, is what settled it.
STEP_REFERENCE_CAPTURE = {
    0x120: 0x3EA0,
    0x122: 0x3E40,
    0x124: 0x51A0,
    0x126: 0x0620,
    0x128: 0x3120,
    0x12A: 0x50C0,
    0x12C: 0x38C0,
    0x12E: 0x5EC0,
    0x130: 0xD080,
}

TEMPERATURE_FRACTION = 256.0

# key -> (row label, min C, max C).
# The high byte is a signed whole degree, so a reading cannot exceed 127 no
# matter what the sensor does. The upper bounds below sit under that on
# purpose: past them the number is a failed read rather than a hot board.
INTEL_TEMPERATURES = {
    "cpu": ("CPU Temp", -20.0, 120.0),
    "vrm": ("VRM Temp", -20.0, 125.0),
    "socket": ("CPU Socket Temp", -20.0, 120.0),
    "pch": ("PCH Temp", -20.0, 120.0),
    "system": ("System Temp", -20.0, 100.0),
}

CONFIRMED_TEMPERATURES = {
    "cpu": 0x100,
    "system": 0x102,
    "vrm": 0x104,
    "pch": 0x106,
    "socket": 0x108,
}

# --- ASUS ROG MAXIMUS Z790 APEX, Nuvoton NCT6798D (chip 0xD42B, config port
# 0x2E, monitor base 0x290).
#
# A different chip family and a different transport; see nct679x. Sensors are
# single bytes addressed as (bank << 8) | register.
#
# Confirmed against HWiNFO reading this same chip on the same boot, and then
# each candidate put through an idle -> all-core load -> idle cycle, because a
# static capture only shows that a number resembles a reading:
#
#   CPU      0x491  33 C idle, 41-42 C loaded, 34 C recovered. HWiNFO's CPU
#                   read 33 C at capture with a 41 C maximum: the same value
#                   at rest and the same ceiling under load.
#   PCH      0x401  49 C, unmoved by an all-core load, against HWiNFO's PCH
#                   49 C. A PCH that tracked CPU load would not be a PCH.
#   System   0x027  31 C, flat, against HWiNFO's Motherboard 31 C. This is the
#                   conventional SYSTIN register on this family.
#
# 0x0FB is deliberately absent. It reads exactly 36, the only register in the
# whole space matching HWiNFO's "CPU Package 36 C", and it was still wrong: it
# did not move one degree across a full load cycle. It is the register this
# mapping would have shipped on resemblance alone.
#
# VRM and CPU Socket have no entry because this chip does not carry them on
# this board. HWiNFO lists no VRM or socket sensor under the NCT6798D; its
# "VR VCC Temperature" comes from SVID telemetry, a different transport
# entirely. Two blank rows are the honest report.
NCT6798D_TEMPERATURES = {
    "cpu": 0x491,
    "pch": 0x401,
    "system": 0x027,
}

# rail key -> (sensor address, volts per count). See nct679x.decode_volts.
#
#   Vcore      0x480  0.009  the only load-tracking channel in the block: it
#                     droops under an all-core load and recovers, which is a
#                     core rail and nothing else.
#
#                     The step is 9 mV, not the 8 mV most of this block uses.
#                     That was got wrong first time by taking the droop as
#                     proof of the channel and assuming the block's usual step
#                     came with it: raw 0x92 -> 1.314 V at 9 mV, exactly
#                     HWiNFO's Vcore for this chip, where 8 mV gives 1.176 V,
#                     seventeen counts adrift and far outside anything
#                     sampling explains. The droop identified the channel; it
#                     said nothing about the scale.
#
#                     9 mV also puts Vcore in the same family as VDD2 below,
#                     whose 18 mV is 2 x 9 and was pinned independently by a
#                     two-count HWiNFO range. This block is not uniformly
#                     8 mV, which is the trap.
#   VDD2       0x48A  0.018  0x4C -> 1.368 V, exactly HWiNFO's IMC VDD, the
#                     memory-controller supply ASUS calls VDD2 in BIOS.
#                     HWiNFO's own 1.332-1.368 V range over the same window is
#                     raw 74-76, two counts, which fixes the step at 18 mV
#                     independently of the single-point match.
#   CPU SA     0x48D  0.016  0x4A -> 1.184 V, exactly HWiNFO's IVR VCCSA.
#                     Distinct from the VCCSA this project reads from MCHBAR
#                     (1.204 V here): VRM-side against on-die, the same rail
#                     measured in two places.
#   CPU AUX    0x48E  0.016  0x73 -> 1.840 V, exactly HWiNFO's VCCIN_AUX.
#
# The block is positional, which is what makes the odd 18 mV step credible
# rather than a fitted number. Reading 0x480-0x48F straight through against
# HWiNFO's list in order gives Vcore, +5V, AVSB, 3VCC, +12V, IVR Atom L2, VIN4,
# 3VSB_ATX, BAT_3V, VTT, IMC VDD, CPU L2, PCH 1.05V, IVR VCCSA, VCCIN_AUX,
# VIN9 -- and six of those land exactly without being claimed as rails here:
# 0x482/0x487 -> 3.376 V, 0x483 -> 3.360 V, 0x488 -> 3.136 V, 0x489 -> 1.040 V,
# 0x48B -> 0.000 V, 0x48C -> 1.056 V.
#
# DRAM has no entry. Nothing in this block carries it: on this board the DIMM
# rails are reported by the DDR5 PMIC, which this project reaches through
# ddr5_pmic instead.
NCT6798D_RAILS = {
    "vcore": (0x480, 0.009),
    # VTT 0x489 0.016 -> raw 0x41 gives 1.040 V and 0x42 gives 1.056 V, both
    # of which HWiNFO reported for this channel across one capture window as
    # its minimum and maximum. Two matched endpoints fix the step.
    "vtt": (0x489, 0.016),
    "vdd2": (0x48A, 0.018),
    "cpu_sa": (0x48D, 0.016),
    "cpu_aux": (0x48E, 0.016),
}

_DETECTED = {}
_PROFILE = []


def _detected_reader(reader_factory):
    """Return a detected reader, or None when this board has no usable chip.

    A refusal is cached alongside a success. Detection drives the ISA path and
    Summary re-reads on every tick, so re-probing a chip that has already
    declined would take the monitoring mutex once a second for an answer that
    cannot change while the machine is running.
    """
    cached = _DETECTED.get(reader_factory)
    if cached is not None:
        return None if cached is False else cached
    reader = reader_factory()
    if not reader.detect():
        _DETECTED[reader_factory] = False
        return None
    # Detection unlocks and re-locks the configuration window, and the chip
    # cannot move while the machine is running, so keep it.
    _DETECTED[reader_factory] = reader
    return reader


def decode_temperature(raw):
    """Decode one sensor word into degrees Celsius."""
    raw = int(raw) & 0xFFFF
    whole = (raw >> 8) & 0xFF
    if whole > 127:
        whole -= 256
    return whole + (raw & 0xFF) / TEMPERATURE_FRACTION


def validate_temperature(key, celsius):
    """Return the reading, or None when it cannot be that sensor."""
    sensor = INTEL_TEMPERATURES.get(key)
    if sensor is None:
        return None
    _label, minimum, maximum = sensor
    try:
        celsius = float(celsius)
    except (TypeError, ValueError):
        return None
    return celsius if minimum <= celsius <= maximum else None


def _nct679x_profile():
    """Return the NCT679x reader and maps, or None when no such chip is here."""
    try:
        from rochviewer.sensors.nct679x import Nct679xReader
    except Exception:
        return None
    reader = _detected_reader(Nct679xReader)
    if reader is None:
        return None
    return {
        "reader": reader,
        "temperatures": NCT6798D_TEMPERATURES,
        "rails": NCT6798D_RAILS,
        "read_temperature": _nct679x_temperature,
        "read_rail": _nct679x_rail,
    }


def _nct668x_profile():
    """Return the NCT668x reader and maps, or None when no such chip is here."""
    try:
        from rochviewer.sensors.superio_lpc import SuperIoReader
    except Exception:
        return None
    reader = _detected_reader(SuperIoReader)
    if reader is None:
        return None
    return {
        "reader": reader,
        "temperatures": CONFIRMED_TEMPERATURES,
        "rails": CONFIRMED_RAILS,
        "read_temperature": _nct668x_temperature,
        "read_rail": _nct668x_rail,
    }


# --- Gigabyte Z890 AORUS TACHYON ICE, ITE IT8696E (config port 0x2E, EC
# window 0x0A40) and IT87952E (0x4E, 0x0B10).
#
# A third chip family. ITE answers its own unlock sequence and reads sensors
# as single bytes through an index/data pair, so neither Nuvoton transport
# sees it and every row on this board read blank.
#
# Confirmed against HWiNFO reading these same chips on the same boot. The two
# parts do not share a voltage LSB, which is the detail that makes or breaks
# the decode -- IT8696E steps 12 mV, IT87952E steps 10.9 mV, and swapping them
# misses every rail by 8-10%:
#
#   VCCSA    IT8696E  0x26  109 x 12.0 mV -> 1.308 V.  HWiNFO VCCSA 1.308.
#   VDD2     IT87952E 0x21  101 x 10.9 mV -> 1.101 V.  HWiNFO CPU VDD2 1.111.
#
# Two channels corroborate the IT87952E step without being claimed as rails:
# 0x22 decodes to 0.807 V against HWiNFO's "PCH 0.82V" 0.814, and 0x25 to
# 1.798 V against its "PCH 1.8V" 1.815. On IT8696E, 0x24 lands exactly on
# iGPU VAXG 0.036 and 0x25 exactly on CPU VNNAON 0.768.
#
# Vcore is matched by value and band rather than by a load sweep: it read
# 1.428 V inside HWiNFO's own 1.020-1.464 V range for CPU DLVRin Vcore, and
# no other channel on either chip sits in that band. Worth re-confirming
# under load, the way the NCT6798D rails were.
#
# The rail map is board-specific and does not transfer to another model.
ITE_RAILS = {
    "cpu_sa": ("IT8696E", 0x26, 1.0),
    "vdd2": ("IT87952E", 0x21, 1.0),
    "vcore": ("IT8696E", 0x20, 1.0),
    "vnnaon": ("IT8696E", 0x25, 1.0),
    "vccio": ("IT87952E", 0x24, 1.0),
}

# HWiNFO's IT8696E block lists System1, PCH, CPU, PCIEX16 and VRM MOS, which
# is the order the registers appear in. PCIEX16 has no row on the Sensors tab
# and is read past rather than displayed.
ITE_TEMPERATURES = {
    "system": ("IT8696E", 0x29),
    "pch": ("IT8696E", 0x2A),
    "cpu": ("IT8696E", 0x2B),
    "vrm": ("IT8696E", 0x2D),
}


def _ite_profile():
    """Return the ITE reader and maps, or None when no ITE chip is here."""
    try:
        from rochviewer.sensors.ite_superio import IteSuperIoReader
    except Exception:
        return None
    try:
        reader = IteSuperIoReader()
        if not reader.detect():
            return None
    except Exception:
        return None
    return {
        "reader": reader,
        "temperatures": ITE_TEMPERATURES,
        "rails": ITE_RAILS,
        "read_temperature": _ite_temperature,
        "read_rail": _ite_rail,
    }


def _ite_temperature(reader, spec):
    chip, register = spec
    return reader.read_temperature(chip, register)


def _ite_rail(reader, spec):
    chip, register, divider = spec
    return reader.read_voltage(chip, register, divider)


def _nct668x_temperature(reader, address):
    return decode_temperature(reader.read_word(address))


def _nct668x_rail(reader, spec):
    from rochviewer.sensors.superio_lpc import decode_sensor_volts

    address, step = spec
    return decode_sensor_volts(reader.read_word(address), step)


def _nct679x_temperature(reader, address):
    from rochviewer.sensors.nct679x import decode_temperature as decode

    return decode(reader.read_byte(address))


def _nct679x_rail(reader, spec):
    from rochviewer.sensors.nct679x import decode_volts

    address, multiplier = spec
    return decode_volts(reader.read_byte(address), multiplier)


# Tried in order. Each asks its own transport whether its chip is present, so
# a board answers at most one of them; both decline on a chip neither knows,
# which is the outcome that leaves the rows blank rather than invented.
SENSOR_PROFILES = (_nct668x_profile, _nct679x_profile, _ite_profile)


def board_sensor_profile():
    """Return this board's sensor profile, or None. Resolved once."""
    if _PROFILE:
        return _PROFILE[0]
    for build in SENSOR_PROFILES:
        try:
            profile = build()
        except Exception:
            profile = None
        if profile is not None:
            _PROFILE.append(profile)
            return profile
    _PROFILE.append(None)
    return None


def read_board_temperatures(reader_factory=None, sensors=None):
    """Return ``{sensor key: celsius}`` for the confirmed board sensors.

    With no explicit transport, the board's own chip decides which map and
    which decode apply. A caller that supplies ``reader_factory`` is supplying
    the transport too, and keeps the NCT668x word protocol it always had.
    """
    if reader_factory is None and sensors is None:
        profile = board_sensor_profile()
        if profile is None:
            return {}
        return _read_temperatures(
            profile["reader"], profile["temperatures"],
            profile["read_temperature"],
        )

    sensors = CONFIRMED_TEMPERATURES if sensors is None else sensors
    if not sensors:
        return {}
    try:
        from rochviewer.sensors.superio_lpc import SuperIoReader
    except Exception:
        return {}
    reader = _detected_reader(
        SuperIoReader if reader_factory is None else reader_factory
    )
    if reader is None:
        return {}
    return _read_temperatures(reader, sensors, _nct668x_temperature)


def _read_temperatures(reader, sensors, read_one):
    """Decode each mapped sensor, dropping anything that cannot be that row."""
    try:
        values = {}
        for key, address in sensors.items():
            try:
                celsius = read_one(reader, address)
            except Exception:
                continue
            checked = validate_temperature(key, celsius)
            if checked is not None:
                values[key] = checked
        return values
    except Exception:
        return {}


def temperature_text(key, reader_factory=None):
    """Format one board temperature for a table row, or None when unavailable."""
    celsius = read_board_temperatures(reader_factory=reader_factory).get(key)
    return None if celsius is None else f"{celsius:.1f} °C"


def validate_rail(key, volts):
    """Return the reading, or None when it cannot be that rail."""
    rail = INTEL_RAILS.get(key)
    if rail is None:
        return None
    _label, minimum, maximum = rail
    try:
        volts = float(volts)
    except (TypeError, ValueError):
        return None
    return volts if minimum <= volts <= maximum else None


def read_board_rails(reader_factory=None, sensors=None):
    """Return ``{rail key: volts}`` for the confirmed Intel rails.

    Fail-closed like the other privileged transports: a rail that cannot be
    read, or that decodes outside its band, is left out of the result rather
    than reported as a number.
    """
    if reader_factory is None and sensors is None:
        profile = board_sensor_profile()
        if profile is None:
            return {}
        return _read_rails(
            profile["reader"], profile["rails"], profile["read_rail"]
        )

    sensors = CONFIRMED_RAILS if sensors is None else sensors
    if not sensors:
        # Nothing confirmed on this board, so nothing is claimed. Importing the
        # Super I/O machinery here would also open the ISA path for no reason.
        return {}

    try:
        from rochviewer.sensors.superio_lpc import SuperIoReader
    except Exception:
        return {}

    reader = _detected_reader(
        SuperIoReader if reader_factory is None else reader_factory
    )
    if reader is None:
        return {}
    return _read_rails(reader, sensors, _nct668x_rail)


def _read_rails(reader, sensors, read_one):
    """Decode each mapped rail, dropping anything outside its band."""
    try:
        values = {}
        for key, spec in sensors.items():
            try:
                volts = read_one(reader, spec)
            except Exception:
                continue
            checked = validate_rail(key, volts)
            if checked is not None:
                values[key] = checked
        return values
    except Exception:
        return {}


def rail_text(key, reader_factory=None):
    """Format one rail for a table row, or None when it is unavailable."""
    volts = read_board_rails(reader_factory=reader_factory).get(key)
    return None if volts is None else f"{volts:.3f}V"
