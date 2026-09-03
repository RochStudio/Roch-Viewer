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

"""Fail-closed Granite Ridge FCLK/UCLK reader via RSMU PM-table telemetry.

Only the exact validated table version ``0x620105`` is decoded, using the
CONFIRMED offsets from the MIT gnr-smu map:

  FCLK @ 0x11C (float MHz)
  UCLK @ 0x12C (float MHz)
  MCLK @ 0x13C (float MHz, cross-check only)

Flow (fixed addresses / commands only):
  1. CPU gate (desktop Ryzen 9000 Granite Ridge)
  2. RSMU cmd 0x05 -> table version must equal 0x620105
  3. RSMU cmd 0x04 args [1,1,0,0,0,0] -> firmware DRAM base
  4. RSMU cmd 0x03 args all-zero -> transfer table into that base
  5. Read physical floats at the fixed offsets
  6. Validate ranges; optionally cross-check UCLK vs UMC MCLK
"""

from __future__ import annotations

import ctypes
import math
import struct
import threading
import time
from ctypes import wintypes
from dataclasses import dataclass

from rochviewer.hardware.pci_mcfg import (
    InpOutPhysicalAccess,
    ecam_address,
    get_mcfg_table,
    parse_mcfg,
    select_allocation,
)
from rochviewer.amd.smn import (
    AMD_VENDOR_ID,
    DEFAULT_MUTEX_NAME,
    InpOutPortIO,
    NamedMutex,
    RSMU_ARG0,
    RSMU_MSG,
    RSMU_RSP,
    RSMU_TABLE_VERSION_COMMAND,
    SMN_DATA_REG,
    SMN_INDEX_REG,
    VENDOR_REG,
)
from rochviewer.platform_profiles import is_granite_ridge_cpu

RSMU_TABLE_ADDRESS_COMMAND = 0x04
RSMU_TABLE_TRANSFER_COMMAND = 0x03

# GetPBOScalar. The fourth and last permitted command, and the only one that is
# not part of the PM-table sequence.
#
# Provenance: ZenStates-Core's Zen4Settings, which Zen5Settings inherits
# unchanged for the RSMU mailbox. The same class defines this project's other
# three commands and all three mailbox addresses, and those are already
# confirmed working on this hardware, so the message block is the validated
# one rather than a number found somewhere and hoped for. It is a getter: it
# takes no arguments and returns the scalar in arg0.
RSMU_PBO_SCALAR_COMMAND = 0x6D

# Every command this project may issue. Each is a read; nothing here sets a
# limit, a voltage or a frequency, and no write message is reachable at all.
PERMITTED_COMMANDS = (
    RSMU_TABLE_VERSION_COMMAND,
    RSMU_TABLE_ADDRESS_COMMAND,
    RSMU_TABLE_TRANSFER_COMMAND,
    RSMU_PBO_SCALAR_COMMAND,
)
EXPECTED_TABLE_VERSION = 0x620105
PM_TABLE_LENGTH = 0x724
ADDRESS_REQUEST_ARGUMENTS = (1, 1, 0, 0, 0, 0)

OFFSET_FCLK = 0x11C
OFFSET_UCLK = 0x12C
OFFSET_MCLK = 0x13C

FCLK_MIN_MHZ = 600.0
FCLK_MAX_MHZ = 3000.0
UCLK_MIN_MHZ = 600.0
UCLK_MAX_MHZ = 4200.0


@dataclass(frozen=True)
class SmuClocks:
    version: int
    table_base: int
    fclk_mhz: float
    uclk_mhz: float
    mclk_mhz: float


def check_cpu_gate(cpu_name=""):
    """Return "" if this CPU may be read, else the reason it may not.

    The name is the whole gate. It also took family, model and core count,
    each checked only when supplied and never supplied by anything -- so the
    CPUID they needed was a stub returning None, and the gate that read as
    "family 0x1A, model 0x44" was in fact the name test alone. Better one
    check that runs than four that describe an intention.
    """
    if not is_granite_ridge_cpu(str(cpu_name or "")):
        return "CPU is not a validated desktop Ryzen 9000 Granite Ridge part"
    return ""


def decode_table_float(raw_dword, what="value", finite_only=True):
    """Decode one PM-table dword as a little-endian float.

    Every consumer of this table — clocks, voltages, power, and the research
    probes — reads the same 32-bit float format, so they share this decoder.
    ``finite_only=False`` is for the probes, which dump arbitrary dwords and
    must render a non-finite one rather than abort the report.
    """
    value = struct.unpack("<f", struct.pack("<I", int(raw_dword) & 0xFFFFFFFF))[0]
    if finite_only and not math.isfinite(value):
        raise ValueError("non-finite %s float" % what)
    return float(value)


def decode_clock_float(raw_dword):
    return decode_table_float(raw_dword, "clock")


def validate_clocks(fclk, uclk, mclk=None, umc_mclk=None):
    """Return (fclk, uclk) or raise ValueError when values look wrong."""
    fclk = float(fclk)
    uclk = float(uclk)
    if not (FCLK_MIN_MHZ <= fclk <= FCLK_MAX_MHZ):
        raise ValueError("FCLK %.1f MHz out of range" % fclk)
    if not (UCLK_MIN_MHZ <= uclk <= UCLK_MAX_MHZ):
        raise ValueError("UCLK %.1f MHz out of range" % uclk)
    # Prefer near-integer MHz values (telemetry is float but clocks are discrete).
    if abs(fclk - round(fclk)) > 1.5:
        raise ValueError("FCLK %.3f does not look like a fabric clock" % fclk)
    if abs(uclk - round(uclk)) > 1.5:
        raise ValueError("UCLK %.3f does not look like a memory clock" % uclk)
    ref = umc_mclk if umc_mclk is not None else mclk
    if ref is not None:
        ref = float(ref)
        if ref > 0:
            # UCLK is typically MCLK or MCLK/2.
            if min(abs(uclk - ref), abs(uclk - ref / 2.0)) > max(50.0, ref * 0.08):
                raise ValueError(
                    "UCLK %.0f does not match UMC/PM MCLK %.0f (or half)"
                    % (uclk, ref)
                )
    return float(round(fclk)), float(round(uclk))


class InpOutRsmuClockAccess:
    """Fixed-function ECAM mailbox access for version/address/transfer/read."""

    def __init__(self, port_io=None, table=None, segment=0, bus=0):
        self._port_io = port_io or InpOutPortIO()
        self._physical = InpOutPhysicalAccess(self._port_io._dll)
        self._prior_selector = None
        self.last_written_selector = None
        entries = parse_mcfg(table if table is not None else get_mcfg_table())
        allocation = select_allocation(entries, segment, bus)
        self.config_base = ecam_address(allocation, bus, 0, 0, 0)

        raw_write = self._port_io._dll.SetPhysLong
        raw_write.argtypes = [wintypes.LPVOID, wintypes.DWORD]
        raw_write.restype = wintypes.BOOL
        selector_address = self.config_base + SMN_INDEX_REG
        data_address = self.config_base + SMN_DATA_REG

        def write_selector(value):
            value = int(value) & 0xFFFFFFFF
            if not raw_write(ctypes.c_void_p(selector_address), wintypes.DWORD(value)):
                raise OSError(
                    "SetPhysLong failed at selector 0x%016X" % selector_address
                )
            self.last_written_selector = value

        def write_data(value):
            if not raw_write(
                ctypes.c_void_p(data_address),
                wintypes.DWORD(int(value) & 0xFFFFFFFF),
            ):
                raise OSError(
                    "SetPhysLong failed at data window 0x%016X" % data_address
                )

        def read_fixed_smn(address):
            write_selector(address)
            return self._physical.read_dword(data_address) & 0xFFFFFFFF

        def write_fixed_smn(address, value):
            write_selector(address)
            write_data(value)

        self._read_response = lambda: read_fixed_smn(RSMU_RSP)
        self._clear_response = lambda: write_fixed_smn(RSMU_RSP, 0)
        self._write_args = lambda values: [
            write_fixed_smn(RSMU_ARG0 + index * 4, values[index])
            for index in range(6)
        ]
        self._issue = lambda command: write_fixed_smn(RSMU_MSG, command)
        self._read_arg = lambda index: read_fixed_smn(RSMU_ARG0 + index * 4)
        self._restore = lambda: write_selector(self._prior_selector)
        self._read_phys = self._physical.read_dword

    def is_driver_open(self):
        return self._port_io.is_driver_open()

    def read_vendor(self):
        return self._physical.read_dword(self.config_base + VENDOR_REG) & 0xFFFF

    def capture_selector(self):
        self._prior_selector = self._physical.read_dword(
            self.config_base + SMN_INDEX_REG
        ) & 0xFFFFFFFF

    @property
    def selector_captured(self):
        return self._prior_selector is not None

    def read_response(self):
        return self._read_response()

    def clear_response(self):
        self._clear_response()

    def write_arguments(self, values):
        if len(values) != 6:
            raise ValueError("RSMU argument vector must have 6 DWORDs")
        self._write_args([int(v) & 0xFFFFFFFF for v in values])

    def issue_command(self, command):
        command = int(command) & 0xFF
        if command not in PERMITTED_COMMANDS:
            raise ValueError("command 0x%02X is not permitted" % command)
        self._issue(command)

    def read_arg0(self):
        return self._read_arg(0)

    def read_arg1(self):
        return self._read_arg(1)

    def restore_selector(self):
        if self._prior_selector is None:
            raise RuntimeError("No prior SMN selector was captured")
        self._restore()

    def read_phys_dword(self, address):
        return self._read_phys(int(address) & 0xFFFFFFFFFFFFFFFF) & 0xFFFFFFFF


class RsmuClockReader:
    """Read FCLK/UCLK from the approved PM-table version only."""

    def __init__(
        self,
        access,
        mutex=None,
        mutex_name=DEFAULT_MUTEX_NAME,
        monotonic=time.monotonic,
        sleep=time.sleep,
        timeout=0.25,
        total_timeout=1.0,
        umc_mclk_mhz=None,
    ):
        self._access = access
        self._mutex = mutex if mutex is not None else NamedMutex(mutex_name)
        self._lock = threading.Lock()
        self._monotonic = monotonic
        self._sleep = sleep
        timeout = float(timeout)
        total_timeout = float(total_timeout)
        if not math.isfinite(timeout) or not 0.0 < timeout <= 0.25:
            raise ValueError("RSMU timeout must be finite and within (0, 0.25]")
        if not math.isfinite(total_timeout) or not 0.0 < total_timeout <= 1.0:
            raise ValueError("RSMU total timeout must be finite and within (0, 1.0]")
        self._timeout = timeout
        self._total_timeout = total_timeout
        self._umc_mclk_mhz = umc_mclk_mhz
        self._last_monotonic = None
        self._run_deadline = None
        self.last_error = ""

    def _clock_now(self):
        now = float(self._monotonic())
        if not math.isfinite(now):
            raise RuntimeError("RSMU monotonic clock is invalid")
        if self._last_monotonic is not None and now < self._last_monotonic:
            raise RuntimeError("RSMU monotonic clock moved backward")
        self._last_monotonic = now
        if self._run_deadline is not None and now > self._run_deadline:
            raise TimeoutError("RSMU total-run budget expired")
        return now

    def _wait_for_response(self):
        started = self._clock_now()
        deadline = started + self._timeout
        while True:
            before = self._clock_now()
            if before > deadline:
                raise TimeoutError("RSMU mailbox response timed out")
            response = int(self._access.read_response()) & 0xFFFFFFFF
            after = self._clock_now()
            if after > deadline:
                raise TimeoutError("RSMU mailbox response timed out")
            if response != 0:
                return response
            if after >= deadline:
                raise TimeoutError("RSMU mailbox response timed out")
            self._sleep(min(0.001, deadline - after))

    def _command(self, command, arguments=(0, 0, 0, 0, 0, 0)):
        self._wait_for_response()
        self._access.clear_response()
        self._access.write_arguments(arguments)
        self._access.issue_command(command)
        response = self._wait_for_response()
        if response != 1:
            raise RuntimeError(
                "RSMU command 0x%02X returned 0x%08X" % (command, response)
            )

    def read_clocks(self):
        self.last_error = ""
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
                    self._command(
                        RSMU_TABLE_ADDRESS_COMMAND, ADDRESS_REQUEST_ARGUMENTS
                    )
                    arg0 = int(self._access.read_arg0()) & 0xFFFFFFFF
                    arg1 = int(self._access.read_arg1()) & 0xFFFFFFFF
                    base = (arg0 | (arg1 << 32)) & 0xFFFFFFFFFFFFFFFF
                    if base == 0 or base == 0xFFFFFFFFFFFFFFFF:
                        raise ValueError("firmware returned an invalid PM-table base")
                    if base > (1 << 48):
                        raise ValueError("PM-table base exceeds phys address width")
                    if base + PM_TABLE_LENGTH > (1 << 48):
                        raise ValueError("PM-table range is not mappable")
                    self._command(RSMU_TABLE_TRANSFER_COMMAND, (0, 0, 0, 0, 0, 0))
                    fclk = decode_clock_float(
                        self._access.read_phys_dword(base + OFFSET_FCLK)
                    )
                    uclk = decode_clock_float(
                        self._access.read_phys_dword(base + OFFSET_UCLK)
                    )
                    mclk = decode_clock_float(
                        self._access.read_phys_dword(base + OFFSET_MCLK)
                    )
                    fclk, uclk = validate_clocks(
                        fclk,
                        uclk,
                        mclk=mclk,
                        umc_mclk=self._umc_mclk_mhz,
                    )
                    return SmuClocks(
                        version=version,
                        table_base=base,
                        fclk_mhz=fclk,
                        uclk_mhz=uclk,
                        mclk_mhz=float(round(mclk)) if math.isfinite(mclk) else 0.0,
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
                self.last_error = "RSMU clock read failed: %s" % exc
            return None


# Constructing this re-loads the DLL, rebinds ctypes prototypes and re-parses
# the ACPI MCFG table (two GetSystemFirmwareTable calls). None of that can
# change while the machine is running, and the clock, voltage and power readers
# each want one per read, so it is built once and shared.
_SHARED_ACCESS = {"access": None}


def shared_rsmu_access():
    """Return the process-wide RSMU access object, building it on first use."""
    if _SHARED_ACCESS["access"] is None:
        _SHARED_ACCESS["access"] = InpOutRsmuClockAccess()
    return _SHARED_ACCESS["access"]


def read_smu_clocks(cpu_name="", umc_mclk_mhz=None):
    """Convenience entry used by Am5Runtime. Returns SmuClocks or None."""
    reason = check_cpu_gate(cpu_name=cpu_name)
    if reason:
        return None
    try:
        reader = RsmuClockReader(shared_rsmu_access(), umc_mclk_mhz=umc_mclk_mhz)
        return reader.read_clocks()
    except Exception:
        _SHARED_ACCESS["access"] = None      # rebuild next time
        return None
