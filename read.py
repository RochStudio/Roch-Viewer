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

import ctypes
import os
from ctypes import wintypes


from driver_path import find_driver, missing_message

DLL_PATH = find_driver()
DYNAMIC_KEYS = (
    "offset_start",
    "value_to_find",
    "offset_base",
    "bit_start_dynamic",
    "bit_length_dynamic",
    "mchbar",
    "command",
    "offset2",
)

if DLL_PATH is None:
    raise SystemExit("Error: " + missing_message())

try:
    inpout = ctypes.WinDLL(DLL_PATH)
except OSError as exc:
    raise SystemExit(
        "Error loading inpoutx64.dll. Ensure it is compatible and the app "
        "has administrator privileges."
    ) from exc

inpout.MapPhysToLin.argtypes = [
    wintypes.LPVOID,
    wintypes.DWORD,
    ctypes.POINTER(wintypes.HANDLE),
]
inpout.MapPhysToLin.restype = wintypes.LPVOID
inpout.UnmapPhysicalMemory.argtypes = [wintypes.HANDLE, wintypes.LPVOID]
inpout.UnmapPhysicalMemory.restype = wintypes.BOOL


def map_physical_address(phys_addr, size):
    handle = wintypes.HANDLE()
    virt_addr = inpout.MapPhysToLin(phys_addr, size, ctypes.byref(handle))
    if not virt_addr:
        raise RuntimeError(
            f"Failed to map physical address 0x{phys_addr:016X}. "
            "Ensure the driver is running with administrator privileges."
        )
    return virt_addr, handle


def unmap_physical_memory(handle, virt_addr):
    if not inpout.UnmapPhysicalMemory(handle, virt_addr):
        print(f"Warning: Failed to unmap physical memory at 0x{virt_addr:016X}")


def read_physical_memory(phys_addr, size=4):
    try:
        virt_addr, handle = map_physical_address(phys_addr, size)
        try:
            buffer_type = ctypes.c_ubyte * size
            return bytes(buffer_type.from_address(virt_addr))
        finally:
            unmap_physical_memory(handle, virt_addr)
    except Exception as exc:
        print(f"Error reading physical memory at 0x{phys_addr:016X}: {exc}")
        return None


def dynamic_read_physical_memory(
    offset_start,
    value_to_find,
    offset_base,
    bit_start_dynamic,
    bit_length_dynamic,
    mchbar,
    command,
    offset2,
):
    start_address = mchbar + offset_start
    try:
        value_to_find = int(value_to_find) & 0xFF
        for current_address in range(start_address, mchbar + 0xE800, 4):
            data = read_physical_memory(current_address, 4)
            if data is None:
                continue

            data_value = int.from_bytes(data, byteorder="little")
            if (data_value & 0xFF) != value_to_find:
                continue
            if ((data_value >> 22) & 0x3) != command:
                continue

            offset = (data_value >> 8) & 0xFF
            target_address = mchbar + offset_base + offset - offset2
            target_data = read_physical_memory(target_address, 4)
            if target_data is None:
                print(
                    f"Warning: Failed to read target address "
                    f"0x{target_address:016X} for value 0x{value_to_find:02X}"
                )
                continue
            return extract_value_from_hex(
                target_data.hex(), bit_start_dynamic, bit_length_dynamic
            )
        return None
    except Exception as exc:
        print(f"Error in dynamic read at 0x{start_address:016X}: {exc}")
        return None


def extract_value_from_hex(hex_str: str, bit_start: int, bit_width: int) -> int:
    compact_hex = hex_str.replace(" ", "")
    if len(compact_hex) != 8:
        raise ValueError(
            f"Input must be 4 bytes (8 hex chars), got: {compact_hex}"
        )

    width = abs(bit_width)
    raw_value = int.from_bytes(bytes.fromhex(compact_hex), byteorder="little")
    value = (raw_value >> bit_start) & ((1 << width) - 1)
    if bit_width < 0:
        value = int(f"{value:0{width}b}"[::-1], 2)
    return value


def read_timing(
    address=None,
    bit_start=None,
    bit_length=None,
    read_type="standard",
    dynamic_params=None,
):
    try:
        if read_type == "dynamic" and dynamic_params:
            missing = [key for key in DYNAMIC_KEYS if key not in dynamic_params]
            if missing:
                raise ValueError(
                    f"Dynamic read is missing: {', '.join(missing)}"
                )
            return dynamic_read_physical_memory(
                *(dynamic_params[key] for key in DYNAMIC_KEYS)
            )

        if read_type == "standard" and address is not None:
            data = read_physical_memory(address)
            if data is None:
                return None
            return extract_value_from_hex(data.hex(), bit_start, bit_length)

        print(
            f"Invalid read configuration: read_type={read_type}, "
            f"address={address}, dynamic_params={dynamic_params}"
        )
        return None
    except Exception as exc:
        location = f" at 0x{address:016X}" if isinstance(address, int) else ""
        print(f"Error processing memory{location}: {exc}")
        return None


def read_physical_memory_int(phys_addr, size=4):
    """Read a little-endian physical-memory value as an integer."""
    data = read_physical_memory(phys_addr, size)
    return None if data is None else int.from_bytes(data, byteorder="little")
