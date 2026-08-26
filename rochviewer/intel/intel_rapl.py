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

"""Intel package power, from the RAPL energy counters in MCHBAR.

The counterpart of the AM5 profile's PPT/TDC/EDC: what the package is actually
drawing, which is the one thing the Intel Sensors tab could not say.

RAPL is usually reached through MSRs, which this project cannot read -- the
bundled InpOut driver does port I/O and physical-memory mapping, and offers no
RDMSR. The same counters are also published in MCHBAR, which the timing
backend already reads, so no new transport is needed for this at all.

Power is a rate, not a value. The registers are free-running 32-bit energy
accumulators, so a reading is the difference between two samples divided by
the time between them, and the first sample of a session can only be a
baseline.

SAFETY - read-only. Physical reads through the same MCHBAR window the timing
backend already uses. Nothing here writes.
"""

from __future__ import annotations

MCHBAR = 0xFEDC0000

# Unit register: power in bits [3:0], energy in [12:8], time in [19:16], each
# the negative power of two the domain is counted in.
#
# Reads 0x000A0E03 on the Z790 bench: power 1/8 W, energy 1/2^14 J = 61.035 uJ,
# time 1/2^10 s. That energy unit is the standard client value, and it is what
# makes the counters below decode to the right watts rather than a number that
# merely moves in the right direction.
RAPL_POWER_UNIT = 0x5938

# Energy-status accumulators.
#
# Confirmed on ASUS ROG MAXIMUS Z790 APEX + i9-14900KS against HWiNFO reading
# the same machine, and then through an idle -> all-core load -> idle cycle,
# because a counter that merely increases proves nothing on its own:
#
#              idle      loaded    recovered   HWiNFO idle
#   0x593C     43.8 W    193.8 W   43.3 W      CPU Package Power 42.7 W
#   0x5928     34.2 W    183.5 W   33.0 W      IA Cores Power    32.6 W
#
# The pair corroborate each other as well as HWiNFO: package minus cores is
# 9.6 W at idle and 10.3 W loaded, against HWiNFO's System Agent 8.9 W plus
# Rest-of-Chip 0.4 W. A mis-assigned counter would not hold that difference
# across a 4x swing in draw.
ENERGY_STATUS = {
    "package": 0x593C,
    "cores": 0x5928,
}

# A rate outside this band is a failed read or a counter that wrapped more
# than once, not a package draw. Desktop parts do not sustain a kilowatt.
POWER_MIN_W = 0.0
POWER_MAX_W = 1000.0

# Past this the previous sample is too old to subtract from. The counter is
# 32 bits, so at a few hundred watts it wraps in about twenty minutes and a
# stale baseline would give one confident, wrong reading on the way back.
MAX_SAMPLE_AGE_S = 30.0

# domain -> (raw counter, timestamp) from the previous read.
_LAST = {}


def decode_units(raw):
    """Return ``{"power": W, "energy": J, "time": s}`` per count."""
    raw = int(raw) & 0xFFFFFFFF
    return {
        "power": 1.0 / (1 << (raw & 0x0F)),
        "energy": 1.0 / (1 << ((raw >> 8) & 0x1F)),
        "time": 1.0 / (1 << ((raw >> 16) & 0x0F)),
    }


def energy_rate(previous, current, seconds, energy_unit):
    """Watts between two samples of a free-running 32-bit counter.

    The subtraction is masked, so one wrap between samples is handled by the
    arithmetic rather than needing to be detected.
    """
    if seconds <= 0:
        return None
    delta = (int(current) - int(previous)) & 0xFFFFFFFF
    return delta * float(energy_unit) / float(seconds)


def validate_power(watts):
    """Return the reading, or None when it cannot be a package draw."""
    try:
        watts = float(watts)
    except (TypeError, ValueError):
        return None
    return watts if POWER_MIN_W <= watts <= POWER_MAX_W else None


def _default_read_dword():
    from rochviewer.hardware.read import read_physical_memory_int

    def read_dword(address):
        value = read_physical_memory_int(address, 4)
        return None if value is None else int(value) & 0xFFFFFFFF

    return read_dword


def read_power(domain, read_dword=None, monotonic=None):
    """Return watts for one RAPL domain, or None.

    None on the first call of a session: one sample of an accumulator is a
    baseline, not a rate. The row shows nothing for one tick rather than
    showing a total dressed up as a power.
    """
    offset = ENERGY_STATUS.get(domain)
    if offset is None:
        return None
    if read_dword is None:
        read_dword = _default_read_dword()
    if monotonic is None:
        import time

        monotonic = time.monotonic

    try:
        units = read_dword(MCHBAR + RAPL_POWER_UNIT)
        current = read_dword(MCHBAR + offset)
    except Exception:
        return None
    if units is None or current is None or units in (0x00000000, 0xFFFFFFFF):
        return None

    now = monotonic()
    previous = _LAST.get(domain)
    _LAST[domain] = (current, now)
    if previous is None:
        return None
    last_raw, last_time = previous
    elapsed = now - last_time
    if elapsed <= 0 or elapsed > MAX_SAMPLE_AGE_S:
        return None

    return validate_power(
        energy_rate(last_raw, current, elapsed, decode_units(units)["energy"])
    )


def power_text(domain, read_dword=None, monotonic=None):
    """Format one domain for a table row, or None when unavailable."""
    watts = read_power(domain, read_dword=read_dword, monotonic=monotonic)
    return None if watts is None else f"{watts:.1f} W"
