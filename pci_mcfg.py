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

"""Read-only PCIe MCFG/ECAM address resolution.

This module only reads firmware metadata and physical PCI configuration
space. It deliberately contains no physical/configuration write operation.
"""

import ctypes
import struct
from dataclasses import dataclass
from ctypes import wintypes


_ACPI_PROVIDER = int.from_bytes(b"ACPI", "big")
_MCFG_TABLE_ID = int.from_bytes(b"MCFG", "little")
_ACPI_HEADER_SIZE = 36
_MCFG_RESERVED_SIZE = 8
_MCFG_ENTRY_SIZE = 16


@dataclass(frozen=True)
class McfgAllocation:
    base_address: int
    segment: int
    start_bus: int
    end_bus: int


@dataclass(frozen=True)
class McfgVendorProbe:
    allocation: McfgAllocation
    config_address: int
    raw_dword: int
    vendor_id: int
    device_id: int


def parse_mcfg(table):
    data = bytes(table)
    if len(data) < _ACPI_HEADER_SIZE + _MCFG_RESERVED_SIZE:
        raise ValueError("MCFG table is too short")
    if data[:4] != b"MCFG":
        raise ValueError("ACPI table signature is not MCFG")
    declared_length = struct.unpack_from("<I", data, 4)[0]
    if declared_length < _ACPI_HEADER_SIZE + _MCFG_RESERVED_SIZE:
        raise ValueError("MCFG declared length is invalid")
    if declared_length > len(data):
        raise ValueError("MCFG table is truncated")
    if (
        declared_length - _ACPI_HEADER_SIZE - _MCFG_RESERVED_SIZE
    ) % _MCFG_ENTRY_SIZE:
        raise ValueError("MCFG allocation area is misaligned")
    usable_length = declared_length
    entries = []
    offset = _ACPI_HEADER_SIZE + _MCFG_RESERVED_SIZE
    while offset + _MCFG_ENTRY_SIZE <= usable_length:
        base, segment, start_bus, end_bus, _reserved = struct.unpack_from(
            "<QHBBI", data, offset
        )
        if (
            start_bus <= end_bus
            and base != 0
            and base & ((1 << 20) - 1) == 0
        ):
            entries.append(
                McfgAllocation(base, segment, start_bus, end_bus)
            )
        offset += _MCFG_ENTRY_SIZE
    if not entries:
        raise ValueError("MCFG contains no allocation entries")
    return tuple(entries)


def select_allocation(entries, segment=0, bus=0):
    for entry in entries:
        if (
            entry.segment == segment
            and entry.start_bus <= bus <= entry.end_bus
        ):
            return entry
    raise ValueError(
        "MCFG has no entry for segment %d bus %d" % (segment, bus)
    )


def ecam_address(entry, bus, device, function, register):
    if not entry.start_bus <= bus <= entry.end_bus:
        raise ValueError("bus is outside the MCFG allocation")
    if not 0 <= device <= 31 or not 0 <= function <= 7:
        raise ValueError("invalid PCI device/function")
    if not 0 <= register <= 0xFFF:
        raise ValueError("invalid PCI configuration register")
    return (
        entry.base_address
        + ((bus - entry.start_bus) << 20)
        + (device << 15)
        + (function << 12)
        + register
    )


def probe_mcfg_vendor(table, read_dword, segment=0, bus=0):
    entry = select_allocation(parse_mcfg(table), segment, bus)
    address = ecam_address(entry, bus, 0, 0, 0)
    raw = int(read_dword(address)) & 0xFFFFFFFF
    return McfgVendorProbe(
        entry,
        address,
        raw,
        raw & 0xFFFF,
        (raw >> 16) & 0xFFFF,
    )


def get_mcfg_table(kernel32=None):
    k32 = kernel32 or ctypes.WinDLL("kernel32", use_last_error=True)
    function = k32.GetSystemFirmwareTable
    function.argtypes = [
        wintypes.DWORD,
        wintypes.DWORD,
        wintypes.LPVOID,
        wintypes.DWORD,
    ]
    function.restype = wintypes.UINT
    size = function(_ACPI_PROVIDER, _MCFG_TABLE_ID, None, 0)
    if not size:
        raise OSError(
            ctypes.get_last_error(), "GetSystemFirmwareTable(MCFG) failed"
        )
    buffer = (ctypes.c_ubyte * size)()
    actual = function(
        _ACPI_PROVIDER,
        _MCFG_TABLE_ID,
        ctypes.byref(buffer),
        size,
    )
    if not actual or actual > size:
        raise OSError(
            ctypes.get_last_error(), "Reading the MCFG table failed"
        )
    return bytes(buffer[:actual])


class InpOutPhysicalAccess:
    """32-bit physical access through the already bundled InpOut DLL."""

    def __init__(self, dll):
        self._read = dll.GetPhysLong
        self._read.argtypes = [
            wintypes.LPVOID,
            ctypes.POINTER(wintypes.DWORD),
        ]
        self._read.restype = wintypes.BOOL

    def read_dword(self, address):
        value = wintypes.DWORD()
        if not self._read(ctypes.c_void_p(address), ctypes.byref(value)):
            raise OSError("GetPhysLong failed at 0x%016X" % address)
        return int(value.value)

def make_inpout_physical_reader(dll):
    return InpOutPhysicalAccess(dll).read_dword
