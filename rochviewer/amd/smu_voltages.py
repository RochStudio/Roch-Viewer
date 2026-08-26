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

"""Fail-closed Granite Ridge voltage reader via the RSMU PM-table.

This module reuses the exact mailbox flow and the exact approved table version
(``0x620105``) already validated in :mod:`amd_smu_clocks`; only the decoded
offsets differ.

IMPORTANT — no rail is decoded until its offset has been CONFIRMED on real
hardware, because a wrong offset in a memory-tuning tool prints a
plausible-looking but false voltage, which is strictly worse than printing
nothing.  Each entry in ``CONFIRMED_VOLTAGE_OFFSETS`` carries the evidence that
identified it; rails without evidence stay out of the map and render as an em
dash.  To identify more, run ``COLLECT_VOLTAGE_REPORT.bat`` (see
:mod:`amd_smu_voltage_probe`) and compare captures.

Everything else — UI rows, ranges, formatting, validation — is already wired,
so landing a new rail is a one-line change here.
"""

from __future__ import annotations

from dataclasses import dataclass

from rochviewer.amd.smu_clocks import (
    ADDRESS_REQUEST_ARGUMENTS,
    EXPECTED_TABLE_VERSION,
    shared_rsmu_access,
    PM_TABLE_LENGTH,
    RSMU_PBO_SCALAR_COMMAND,
    RSMU_TABLE_ADDRESS_COMMAND,
    RSMU_TABLE_TRANSFER_COMMAND,
    RSMU_TABLE_VERSION_COMMAND,
    RsmuClockReader,
    check_cpu_gate,
    decode_table_float,
)
from rochviewer.amd.smn import AMD_VENDOR_ID
from rochviewer.sensors.voltage_rails import RAILS_BY_KEY, validate_voltage

# rail key -> PM-table byte offset.  Populate ONLY with probe-confirmed values.
#
# Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + BIOS 1.A21, PM-table 0x620105,
# by capturing the table twice — once idle, once under an all-core load — and
# comparing.  The header decodes as the documented Zen (limit, value) pairs,
# which validates the layout before any rail is read:
#
#   0x008/0x00C  PPT   limit 162 W    value  44.3 ->  143.7 W
#   0x020/0x024  TDC   limit 120 A    value  12.5 ->   98.6 A
#   0x028/0x02C  THM   limit  95 C    value  43.7 ->   81.8 C
#
# 0x044/0x048/0x04C are the SVI3 rail triplet that follows those pairs.
# Each entry was additionally checked over 8 back-to-back captures: a real rail
# either holds steady or drifts by tens of millivolts, never by volts.
# Cross-referenced against ZenTimings 1.43.0 read at capture time.  Every rail
# ZenTimings showed was matched to the table within 2.5 mV; the ones it could
# not be matched to are recorded as absent below.
CONFIRMED_VOLTAGE_OFFSETS = {
    # VSOC as actually DELIVERED.  Confirmed by switching the memory profile,
    # which is the only test that separates the SoC offsets:
    #
    #             8200 MT/s   JEDEC 4800
    #   0x0D4 /
    #   0x14C     1.30000     1.02501     VSOC requested  (ZenTimings shows this)
    #   0x0D8     1.20287     1.01555     VSOC delivered  (this rail)
    #
    # ZenTimings 1.43.0 reads the REQUESTED value, which is why it reported a
    # flat 1.3000 V at 8200 while the chip was really delivering 1.203 V — the
    # one voltage the user independently reports it gets wrong.
    #
    # Do NOT go back to 0x04C.  It was mapped here once because it looked like
    # a noisy live rail bracketed by 0x0D8 and 0x0D4, but every capture behind
    # that reasoning was taken at the same memory profile.  Across profiles it
    # moves the WRONG WAY: 1.24517 V at 8200 -> 1.27690 V at JEDEC, while every
    # real SoC-side value fell.  Whatever 0x04C is, it is not the SoC rail.
    "vddcr_soc": 0x0D8,
    # VDDCR_VDD as SVI3 telemetry, matching what HWiNFO labels
    # "CPU VDDCR_VDD Voltage (SVI3 TFN)".  Previously read from the board's
    # Super I/O sensor, which measures the same rail at the VRM and therefore
    # disagrees by tens of millivolts.
    #
    # The PM table lays the SVI3 rails out in fixed five-dword groups, and the
    # VRM temperatures pin the group boundaries independently:
    #
    #   VDD group   0x0C0 0x0C4 volts | 0x0C8 0x0CC amps | 0x0D0 = 33.0 C
    #   SOC group   0x0D4 0x0D8 volts | 0x0DC 0x0E0 amps | 0x0E4 = 35.8 C
    #
    # HWiNFO reports the VDDCR_VDD VRM at 32 C and the VDDCR_SOC VRM at 36 C,
    # matching those two offsets. VDDCR_SOC is the SECOND dword of its group
    # (0x0D8, the delivered value), so VDDCR_VDD takes the second of its own.
    "vddcr_vdd": 0x0C4,
    # ZenTimings: CLDO VDDP 1.0478 V.  0x434 is the only offset in the table at
    # that value.  Confirmed by BIOS change: setting VDDP to 1.10 V moved this
    # offset 1.04776 -> 1.09806, so the rail behind this row is VDDP.
    "cldo_vddq": 0x434,
    # ZenTimings: VDDG CCD and VDDG IOD both 1.0492 V.  0x40C and 0x414 both
    # read 1.04921 V with zero spread, and nothing else in the table does.
    #
    # The CCD/IOD ORDER was settled by setting them apart in BIOS — IOD to
    # 1.08 V, CCD to 1.06 V — and re-reading:
    #
    #   0x40C  1.04921 -> 1.08216   follows VDDG IOD
    #   0x414  1.04921 -> 1.06250   follows VDDG CCD
    #
    # That is the REVERSE of the AMD-convention ordering used while both rails
    # sat at the same value, which is why it was marked unverified until a
    # capture could actually separate them.
    "vddg_iod": 0x40C,
    "vddg_ccd": 0x414,
    # ZenTimings: VDD MISC 1.1000 V.  0x0E8 (duplicated at 0x0EC) reads
    # 1.09999 V with zero spread.  These two offsets were previously mistaken
    # for the VDDG pair purely because 1.100 V looked like a typical VDDG value.
    "cdd_misc": 0x0E8,
    # Deliberately NOT mapped:
    #
    #   dram_vdd / dram_vddq / dram_vpp — NOT PRESENT IN THIS TABLE AT ALL.
    #     Proven twice.  First, DRAM VDD was raised 1.40 -> 1.45 V and VDDQ
    #     1.40 -> 1.42 V in BIOS, and after the reboot not one dword in the
    #     0x724-byte table changed.  Second, ZenTimings read MEM VDD 1.4550 V,
    #     MEM VDDQ 1.4250 V and MEM VPP 1.8000 V while a capture was taken, and
    #     none of those values appears anywhere in the table.  These are DIMM
    #     PMIC rails; ZenTimings gets them over SMBus from the SPD hub.
    #     Reading them here needs that same SMBus path — no offset will do it.
    #
    #   vddio_mem — ABSENT, proven by direct manipulation.  CPU VDDIO was
    #     raised 1.44 -> 1.47 V in BIOS (+30 mV, chosen as ~15x the 2 mV noise
    #     on these offsets) and after the reboot NOT ONE of the 457 dwords rose
    #     by 20-40 mV.  The cluster that looked like VDDIO behaved as follows:
    #
    #       0x0A8/0x0B4/0x0B8/0x0BC   1.3971 -> 1.3971   did not move at all
    #       0x048/0x09C/0x0A0         1.3943 -> 1.3842   moved the WRONG WAY
    #
    #     It is also not a DDR5 PMIC rail: the PMIC exposes only SWA/SWB/SWC,
    #     VIN_Bulk and the 1.8V/1.0V VOUT rails.
    #
    #     The remaining source is the board's Super I/O sensor chip, which is
    #     where HWiNFO's motherboard-section "CPU VDDIO" reading comes from.
    #     That needs an LPC/Super-I/O reader and per-board scaling; no offset
    #     in this table will ever produce it.
    #
    #   vddcr_vdd — 0x044 looked like the core rail in a single idle/load diff
    #     (1.0876 -> 0.5261 V), but repeated sampling showed it ranging
    #     1.34-4.21 V.  It is not a voltage.  No other offset was isolated as
    #     the core rail.
    #
    #   cpu_vid, cldo_vddq, cdd_misc — no offset isolated yet.
}


@dataclass(frozen=True)
class SmuVoltages:
    version: int
    table_base: int
    values: dict          # rail key -> volts (only confirmed, in-range rails)


def decode_voltage_float(raw_dword):
    """Decode one PM-table dword as a little-endian float in volts."""
    return decode_table_float(raw_dword, "voltage")


def decode_voltages(read_dword, base, offsets=None):
    """Decode confirmed rails from a transferred table.

    ``read_dword`` takes an absolute physical address.  Rails that decode out
    of range are dropped rather than displayed, keeping the module fail-closed.
    """
    offsets = CONFIRMED_VOLTAGE_OFFSETS if offsets is None else offsets
    values = {}
    for key, offset in offsets.items():
        rail = RAILS_BY_KEY.get(key)
        if rail is None:
            continue
        try:
            volts = decode_voltage_float(read_dword(base + int(offset)))
            values[key] = validate_voltage(rail, volts)
        except (ValueError, OSError, OverflowError):
            continue
    return values


class RsmuVoltageReader(RsmuClockReader):
    """Read confirmed voltage rails from the approved PM-table version only.

    Inherits the validated mailbox timing, mutex and selector-restore handling
    from :class:`RsmuClockReader` so the privileged sequence stays identical.
    """

    def read_transferred_table(self, decode):
        """Run the approved mailbox sequence, then hand the table to ``decode``.

        ``decode(read_dword, base)`` is called once, after the version gate and
        the transfer command, and must return the caller's result.  Anything it
        raises is treated as a failed read.  Returns ``(version, base, result)``
        or ``None``; ``last_error`` explains a ``None``.

        Shared by every PM-table consumer so the privileged sequence — vendor
        check, selector capture, version gate, bounded transfer, guaranteed
        selector restore — exists in exactly one place.
        """
        self._last_monotonic = None
        self._run_deadline = None
        try:
            if not self._access.is_driver_open():
                raise OSError("InpOut driver is not open")
            with self._lock, self._mutex:
                vendor = int(self._access.read_vendor()) & 0xFFFF
                if vendor != AMD_VENDOR_ID:
                    raise ValueError(
                        "MCFG PCI 00:00.0 vendor is 0x%04X, expected AMD 0x1022"
                        % vendor
                    )
                self._access.capture_selector()
                if not self._access.selector_captured:
                    raise RuntimeError("prior SMN selector was not captured")
                try:
                    self._run_deadline = self._clock_now() + self._total_timeout
                    self._command(RSMU_TABLE_VERSION_COMMAND, (0, 0, 0, 0, 0, 0))
                    version = int(self._access.read_arg0()) & 0xFFFFFFFF
                    if version != EXPECTED_TABLE_VERSION:
                        raise ValueError(
                            "PM-table version 0x%06X is not the approved 0x%06X"
                            % (version, EXPECTED_TABLE_VERSION)
                        )
                    base = self._request_table_base()
                    self._command(RSMU_TABLE_TRANSFER_COMMAND, (0, 0, 0, 0, 0, 0))
                    return version, base, decode(
                        self._access.read_phys_dword, base
                    )
                finally:
                    try:
                        self._access.restore_selector()
                    except Exception as exc:
                        self.last_error = (
                            "CRITICAL: SMN selector restore failed (%s)" % exc
                        )
                        raise
        except Exception as exc:
            if not self.last_error:
                self.last_error = "RSMU PM-table read failed: %s" % exc
            return None

    def read_voltages(self, offsets=None):
        self.last_error = ""
        offsets = CONFIRMED_VOLTAGE_OFFSETS if offsets is None else offsets
        if not offsets:
            self.last_error = (
                "No PM-table voltage offset has been confirmed for this platform"
            )
            return None

        def decode(read_dword, base):
            values = decode_voltages(read_dword, base, offsets)
            if not values:
                raise ValueError("no rail decoded inside its valid range")
            return values

        result = self.read_transferred_table(decode)
        if result is None:
            if not self.last_error:
                self.last_error = "RSMU voltage read failed"
            return None
        version, base, values = result
        return SmuVoltages(version=version, table_base=base, values=values)

    def pbo_scalar_in_run(self):
        """Ask the SMU for the PBO scalar. Valid only inside an active run.

        Named for the constraint: it must be called from the ``decode``
        callback of :meth:`read_transferred_table`, so it rides on that run's
        mailbox session — selector captured, mutex held, deadline armed —
        rather than opening a second privileged sequence for one number.

        The scalar is not in the PM table. ZenStates reads it the same way,
        through the getter at :data:`RSMU_PBO_SCALAR_COMMAND`. Firmware that
        does not accept the message answers with a non-OK status, which raises
        out of ``_command``, so an unrecognised command leaves the row blank
        instead of decoding whatever happened to be in arg0.
        """
        self._command(RSMU_PBO_SCALAR_COMMAND, (0, 0, 0, 0, 0, 0))
        return decode_table_float(
            int(self._access.read_arg0()) & 0xFFFFFFFF, "scalar"
        )

    def _request_table_base(self):
        """Ask firmware for the PM-table base and bound-check it."""
        self._command(RSMU_TABLE_ADDRESS_COMMAND, ADDRESS_REQUEST_ARGUMENTS)
        arg0 = int(self._access.read_arg0()) & 0xFFFFFFFF
        arg1 = int(self._access.read_arg1()) & 0xFFFFFFFF
        base = (arg0 | (arg1 << 32)) & 0xFFFFFFFFFFFFFFFF
        if base == 0 or base == 0xFFFFFFFFFFFFFFFF:
            raise ValueError("firmware returned an invalid PM-table base")
        if base > (1 << 48):
            raise ValueError("PM-table base exceeds phys address width")
        if base + PM_TABLE_LENGTH > (1 << 48):
            raise ValueError("PM-table range is not mappable")
        return base


def read_smu_voltages(cpu_name=""):
    """Convenience entry used by Am5Runtime. Returns SmuVoltages or None."""
    if not CONFIRMED_VOLTAGE_OFFSETS:
        return None
    reason = check_cpu_gate(cpu_name=cpu_name)
    if reason:
        return None
    try:
        return RsmuVoltageReader(shared_rsmu_access()).read_voltages()
    except Exception:
        return None
