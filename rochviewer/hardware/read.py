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
from ctypes import wintypes


from rochviewer.hardware.driver_path import find_driver, missing_message

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

# The driver's absence is a condition, not a crash.
#
# This used to raise SystemExit here, which made the module unimportable
# without the driver present -- and since everything imports it, the whole
# program and the whole test suite went with it. That is wrong twice over:
# a machine with no driver should get a viewer that says so rather than a
# process that dies before drawing a window, and the tests that never touch
# hardware should run anywhere, which is the only way continuous integration
# can run them at all.
#
# So the failure moves to the first read. DRIVER_ERROR carries why, for
# anything that wants to explain itself; every reader here already returns
# None when a read does not work, and every caller already handles None.
inpout = None
DRIVER_ERROR = None

if DLL_PATH is None:
    DRIVER_ERROR = missing_message()
else:
    try:
        inpout = ctypes.WinDLL(DLL_PATH)
    except OSError as exc:
        DRIVER_ERROR = (
            "Found %s but could not load it. It may be the wrong architecture, "
            "or the process may not be running as administrator (%s)."
            % (DLL_PATH, exc)
        )

if inpout is not None:
    inpout.MapPhysToLin.argtypes = [
        wintypes.LPVOID,
        wintypes.DWORD,
        ctypes.POINTER(wintypes.HANDLE),
    ]
    inpout.MapPhysToLin.restype = wintypes.LPVOID
    inpout.UnmapPhysicalMemory.argtypes = [wintypes.HANDLE, wintypes.LPVOID]
    inpout.UnmapPhysicalMemory.restype = wintypes.BOOL


def map_physical_address(phys_addr, size):
    if inpout is None:
        return None, None
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
    # Quietly, when there is no driver at all. The rows already show nothing
    # for a reading that did not work, and a machine without the driver would
    # otherwise print this same line for every register on every refresh --
    # thousands a minute saying one thing.
    if inpout is None:
        return None
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


# An all-ones dword is not a value; it is the absence of a device.
#
# Reading unmapped MMIO succeeds and returns 0xFFFFFFFF, so a field decoded
# out of it produces a number rather than a gap: 0x2CE8 is unmapped on Core
# Ultra 200S, and the eleven drive-strength rows reading it each showed 255 --
# a plausible-looking level that no register ever stated. This is already how
# the rest of the project reads all-ones: _read_ddr_ptm_control() returns None
# on it, and the MCHBAR window is found by where the reads turn to 0xFFFFFFFF.
# Applying the same rule at the read makes those rows report nothing instead
# of reporting a mask.
#
# Scoped to a fully-set dword rather than a fully-set field, deliberately. A
# narrow field of all ones is an ordinary value -- tXSR legitimately reads
# 0x3FF out of a register holding 0x000003FF -- and only the whole dword being
# set carries the "nobody answered" meaning.
ALL_ONES_DWORD = 0xFFFFFFFF


def _dword_is_unmapped(data):
    return len(data) == 4 and int.from_bytes(data, "little") == ALL_ONES_DWORD


# Some memory-controller fields straddle the 32-bit boundary.
#
# 0xE050 on Core Ultra 200S is the case that forced this: tWRPDEN occupies
# bits 27..36, so neither the dword at 0xE050 nor the one at 0xE054 contains
# it. extract_value_from_hex reads exactly four bytes by contract, which made
# such a field unreadable no matter which bit positions were tried -- the
# register was not mis-decoded, it was unreachable. Reading the pair as one
# 64-bit little-endian quantity is the whole fix.
WIDE_READ_BYTES = 8


def extract_value_from_wide_hex(
    hex_str: str, bit_start: int, bit_width: int
) -> int:
    compact_hex = hex_str.replace(" ", "")
    if len(compact_hex) != WIDE_READ_BYTES * 2:
        raise ValueError(
            f"Input must be {WIDE_READ_BYTES} bytes "
            f"({WIDE_READ_BYTES * 2} hex chars), got: {compact_hex}"
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
            if data is None or _dword_is_unmapped(data):
                return None
            return extract_value_from_hex(data.hex(), bit_start, bit_length)

        if read_type == "wide" and address is not None:
            data = read_physical_memory(address, WIDE_READ_BYTES)
            if data is None:
                return None
            return extract_value_from_wide_hex(
                data.hex(), bit_start, bit_length
            )

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
