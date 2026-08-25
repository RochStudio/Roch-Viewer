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

"""Fail-closed Granite Ridge PPT/TDC/EDC reader via the RSMU PM-table.

READ-ONLY. Unlike ZenStates, which writes these limits through the SMU, this
module only decodes them. No write command exists here and none of the RSMU
write messages are reachable from this path — the reader inherits the same
permitted commands as the clock and voltage readers: 0x05 version, 0x04
address, 0x03 transfer, and 0x6D, the PBO-scalar getter. All four are reads.

Offsets follow the same evidence rule as :mod:`amd_smu_voltages`: nothing is
decoded until it has been matched against a known-good reference on real
hardware.
"""

from __future__ import annotations

from dataclasses import dataclass

from rochviewer.amd.power_metrics import (  # noqa: F401  (re-exported for callers)
    METRICS,
    METRICS_BY_KEY,
    PowerMetric,
    format_power,
    validate_power,
)
from rochviewer.amd.smu_clocks import (
    shared_rsmu_access,
    check_cpu_gate,
    decode_table_float,
)
from rochviewer.amd.smu_voltages import RsmuVoltageReader

# metric key -> (value offset, limit offset).  Either may be None.
#
# Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + BIOS 1.A21, PM-table
# 0x620105, by matching ZenStates 2.0.0 (AM5) against the table and checking
# that the paired value offset moves under an all-core load:
#
#   PPT  limit 0x008 = 162 W exactly   value 0x00C   41.0 ->  143.7 W
#   TDC  limit 0x020 = 120 A exactly   value 0x024    9.7 ->   98.6 A
#   EDC  limit 0x0FC = 180 A exactly   value 0x100   38.0 ->  100.0 A
#
# The limits match the ZenStates page exactly and hold steady under load, while
# each paired offset tracks the load — the documented Zen (limit, value)
# pairing.  0x028/0x02C is the same shape for the thermal limit, which reads
# 90.0 C here; see CONFIRMED_TEMPERATURE_OFFSETS.
CONFIRMED_POWER_OFFSETS = {
    "ppt": (0x00C, 0x008),
    "tdc": (0x024, 0x020),
    "edc": (0x100, 0x0FC),
    # scalar — deliberately absent, and it is not an oversight.  0x060 and
    # 0x064 both hold exactly 1.0000, which is far too common a float to claim
    # on a value match.  The scalar does not need the table at all: it has its
    # own SMU getter, which is how ZenStates reads it, so RsmuPowerReader asks
    # firmware directly rather than guessing at an offset.  See
    # RsmuVoltageReader.pbo_scalar_in_run.
}


# sensor key -> PM-table offset, in degrees Celsius.
#
# Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D:
#   0x02C  CPU (Tctl)   43.7 -> 81.8 C under an all-core load; paired with the
#                       95 C thermal limit at 0x028, the documented Zen
#                       (limit, value) shape.
#   0x0D0  VDDCR_VDD VRM  33.0 C against HWiNFO's 32 C
#   0x0E4  VDDCR_SOC VRM  35.8 C against HWiNFO's 36 C
#
# The two VRM sensors are what pinned the SVI3 voltage group boundaries, so
# they are the same evidence that placed VDDCR_VDD at 0x0C4.
#   0x1A8  IOD Average    38.92 against HWiNFO's 38.1 minimum on a cooled
#                       idle, rising 4.0 under an all-core load against its
#                       3.3. The weakest of the three: its loaded peak read
#                       43.5 against 41.4, further out than the sampling bias
#                       below explains. It is the only candidate in the table
#                       with that behaviour, and it sits below the hotspot,
#                       which it must.
#   0x458  IOD Hotspot    42.21 against 41.8 cooled, 46.1 against 45.8 loaded,
#                       rising 3.5 against 4.0.
#   0x700  L3             29.80 against 29.2 cooled, 46.2 against 46.0 loaded,
#                       rising 16.3 against 16.8 -- a climb nothing else in
#                       the table comes near, which is what makes this one
#                       unmistakable.
#
# All three were matched on the rise as well as on both ends, because absolute
# values alone picked the wrong offset once: 0x458 peaks within 0.1 of the L3
# maximum and was briefly taken for it, until the idle end showed it climbing
# 3.5 where L3 climbs 16.8.
#
# The comparison is calibrated by the sensors already confirmed. Read in the
# same episode, they land at Tctl -0.70, VDD VRM -0.20, SOC VRM -0.10 and
# MISC VRM 0.00 of HWiNFO's own figures, so a candidate inside that band is
# agreeing as closely as a known-good row does. A peak read against a fresh
# reset runs about a degree high, since this samples at ~35 Hz and HWiNFO
# polls every couple of seconds.
#
#   0x0F8  VDD_MISC VRM   confirmed three times against HWiNFO's own row:
#                       32.00 against 32.0 at idle, 35.00 against 35.0 at the
#                       peak of an all-core load, and 35.00 again on a second
#                       load. It is the flattest sensor in the table, which is
#                       why one match would not have been enough.
CONFIRMED_TEMPERATURE_OFFSETS = {
    "cpu": 0x02C,
    "vdd_vrm": 0x0D0,
    "soc_vrm": 0x0E4,
    "misc_vrm": 0x0F8,
    "iod_average": 0x1A8,
    "iod_hotspot": 0x458,
    "l3": 0x700,
    # The limit Tctl is measured against, the paired half of 0x02C. Read
    # rather than assumed: this bench reports 90.0, and HWiNFO's "Thermal
    # Limit 45.3%" is 40.8/90 to the decimal. A hardcoded 95 would have shown
    # five degrees of headroom that are not there.
    "cpu_limit": 0x028,
}

# A silicon sensor outside this band is a decode error, not a reading.
TEMPERATURE_MIN_C = 0.0
TEMPERATURE_MAX_C = 125.0


@dataclass(frozen=True)
class SmuPower:
    version: int
    table_base: int
    values: dict          # metric key -> current reading
    limits: dict          # metric key -> configured limit
    temperatures: dict    # sensor key -> degrees Celsius


def decode_power_float(raw_dword):
    """Decode one PM-table dword as a little-endian float."""
    return decode_table_float(raw_dword, "power")


def _decode_one(read_dword, base, metric, offset):
    if offset is None:
        return None
    try:
        return validate_power(
            metric, decode_power_float(read_dword(base + int(offset)))
        )
    except (ValueError, OSError, OverflowError):
        return None


def decode_temperatures(read_dword, base, offsets=None):
    """Decode the confirmed PM-table temperature sensors, in Celsius."""
    offsets = CONFIRMED_TEMPERATURE_OFFSETS if offsets is None else offsets
    values = {}
    for key, offset in offsets.items():
        try:
            celsius = decode_table_float(
                read_dword(base + int(offset)), "temperature"
            )
        except (ValueError, OSError, OverflowError):
            continue
        if TEMPERATURE_MIN_C <= celsius <= TEMPERATURE_MAX_C:
            values[key] = celsius
    return values


def decode_power(read_dword, base, offsets=None):
    """Decode confirmed metrics from a transferred table.

    Returns ``(values, limits)``.  Anything that decodes outside its declared
    range is dropped rather than displayed, keeping the module fail-closed.
    """
    offsets = CONFIRMED_POWER_OFFSETS if offsets is None else offsets
    values, limits = {}, {}
    for key, pair in offsets.items():
        metric = METRICS_BY_KEY.get(key)
        if metric is None:
            continue
        value_offset, limit_offset = pair
        value = _decode_one(read_dword, base, metric, value_offset)
        limit = _decode_one(read_dword, base, metric, limit_offset)
        if value is not None:
            values[key] = value
        if limit is not None:
            limits[key] = limit
    return values, limits


class RsmuPowerReader(RsmuVoltageReader):
    """Read confirmed power limits from the approved PM-table version only."""

    def read_power(self, offsets=None):
        self.last_error = ""
        offsets = CONFIRMED_POWER_OFFSETS if offsets is None else offsets
        if not offsets:
            self.last_error = (
                "No PM-table power offset has been confirmed for this platform"
            )
            return None

        def decode(read_dword, base):
            values, limits = decode_power(read_dword, base, offsets)
            if not values and not limits:
                raise ValueError("no metric decoded inside its valid range")
            # The scalar is asked for last, after the table values are already
            # in hand, so a firmware that will not answer the getter costs the
            # scalar row and nothing else.
            scalar = self._read_scalar()
            if scalar is not None:
                values["scalar"] = scalar
            # Temperatures ride along on the same transfer rather than costing
            # a second mailbox sequence.
            return values, limits, decode_temperatures(read_dword, base)

        result = self.read_transferred_table(decode)
        if result is None:
            if not self.last_error:
                self.last_error = "RSMU power read failed"
            return None
        version, base, (values, limits, temperatures) = result
        return SmuPower(
            version=version, table_base=base, values=values, limits=limits,
            temperatures=temperatures,
        )

    def _read_scalar(self):
        """PBO scalar, from the SMU getter rather than the PM table.

        Any failure — a firmware that rejects the message, a timeout, a value
        outside the metric's range — blanks the scalar and leaves every other
        reading on the run intact.
        """
        metric = METRICS_BY_KEY.get("scalar")
        if metric is None:
            return None
        try:
            return validate_power(metric, self.pbo_scalar_in_run())
        except Exception:
            return None


def read_smu_power(cpu_name=""):
    """Convenience entry used by Am5Runtime. Returns SmuPower or None."""
    if not CONFIRMED_POWER_OFFSETS:
        return None
    reason = check_cpu_gate(cpu_name=cpu_name)
    if reason:
        return None
    try:
        return RsmuPowerReader(shared_rsmu_access()).read_power()
    except Exception:
        return None
