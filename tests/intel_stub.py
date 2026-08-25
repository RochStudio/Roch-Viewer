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

"""Import ``intel_timings`` against stubs, for tests that need the built table.

The real ``read`` module loads inpoutx64.dll on import, and every getter in the
Intel profile reads physical memory while the table is being built. Both are
replaced here so the tests touch no hardware and pass on a machine that is not
the Intel target.

Not named ``test_*``: this is a fixture, not a test module.
"""

import importlib
import sys
import types

MCHBAR = 0xFEDC0000
MCHBAR2 = 0xFEDD0000
MC_WINDOW = MCHBAR2 - MCHBAR

# Addresses the stub was asked for, newest last. Tests clear this before acting.
READS = []

# The same reads with their bit fields, for tests that care which field of a
# register was taken rather than only which register.
FIELD_READS = []

# Set to a callable to control what a standard read returns, for a test that
# needs a specific decode. Signature: (address, bit_start, bit_length).
response = None

from platform_profiles import LGA1700_DDR4

_SAVED_MODULES = {}
# [(timings module, the ACTIVE_PLATFORM it had)] so restore puts it back.
_SAVED_ACTIVE_PLATFORM = []


def read_timing(address=None, bit_start=None, bit_length=None,
                read_type="standard", dynamic_params=None):
    if read_type == "dynamic" and dynamic_params:
        READS.append(dynamic_params.get("mchbar"))
        return 8
    READS.append(address)
    FIELD_READS.append((address, bit_start, bit_length))
    if response is not None:
        return response(address, bit_start, bit_length)
    # Any small in-range number: most tests assert on which address was read,
    # never on the decoded value.
    return None if address is None else 5


class _FakeMemoryModule:
    def __init__(self, tag, device_locator):
        self.Tag = tag
        # The board's own socket name, which is what slot labels come from.
        # The reference target reports its two modules as records 1 and 3 while
        # naming them DIMMA2 and DIMMB2, so the stub says the same.
        self.DeviceLocator = device_locator
        self.BankLabel = "BANK 0"
        self.PartNumber = "F4-3600C14-16GVKA"
        self.Manufacturer = "G Skill Intl"
        self.Capacity = str(16 * 1024 ** 3)
        self.Attributes = 2          # dual rank
        self.Speed = 3600
        self.ConfiguredClockSpeed = 3600
        self.SMBIOSMemoryType = 26   # DDR4


class FakeWMI:
    """A populated dual-channel DDR4 Z790 board, matching the beta's target."""

    def Win32_PhysicalMemoryArray(self):
        return [types.SimpleNamespace(MemoryDevices=4)]

    def Win32_PhysicalMemory(self):
        return [
            _FakeMemoryModule("Physical Memory 1", "Controller0-DIMMA2"),
            _FakeMemoryModule("Physical Memory 3", "Controller1-DIMMB2"),
        ]

    def Win32_Processor(self):
        return [types.SimpleNamespace(
            Name="Intel(R) Core(TM) i9-14900KF",
            Manufacturer="GenuineIntel",
            NumberOfCores=8,
            NumberOfLogicalProcessors=32,
        )]

    def Win32_BaseBoard(self):
        return [types.SimpleNamespace(
            Product="PRO Z790-P WIFI DDR4 (MS-7E06)",
            Manufacturer="Micro-Star International Co., Ltd.",
            Version="1.0",
        )]

    def Win32_BIOS(self):
        return [types.SimpleNamespace(
            SMBIOSBIOSVersion="1.H0", Manufacturer="American Megatrends",
        )]

    def Win32_ComputerSystem(self):
        return [types.SimpleNamespace(
            TotalPhysicalMemory=str(32 * 1024 ** 3)
        )]

    def Win32_OperatingSystem(self):
        return [types.SimpleNamespace(
            Caption="Microsoft Windows 11 Pro", BuildNumber="22631",
            OSArchitecture="64-bit",
        )]

    def Win32_VideoController(self):
        return [
            types.SimpleNamespace(Name="NVIDIA GeForce RTX 4070 Ti"),
            # Must be filtered out of the GPU row.
            types.SimpleNamespace(Name="Microsoft Basic Display Adapter"),
        ]

    def __getattr__(self, name):
        return lambda *args, **kwargs: []


def install():
    """Stub the hardware modules and return a freshly imported intel_timings."""
    for name in ("read", "wmi", "intel_timings", "dimm_inventory"):
        _SAVED_MODULES[name] = sys.modules.get(name)

    read_stub = types.ModuleType("read")
    read_stub.read_timing = read_timing
    read_stub.read_physical_memory_int = lambda phys_addr, size=4: 0
    sys.modules["read"] = read_stub

    wmi_stub = types.ModuleType("wmi")
    wmi_stub.WMI = lambda *args, **kwargs: FakeWMI()
    sys.modules["wmi"] = wmi_stub

    # active_platform() prefers whatever timings has already resolved over
    # re-probing WMI, so leaving that global alone would have the backend
    # describing the real host while every other reading came from the fake
    # WMI above. Stub it to match what this stub describes.
    timings = sys.modules.get("timings")
    if timings is not None:
        _SAVED_ACTIVE_PLATFORM.append(
            (timings, getattr(timings, "ACTIVE_PLATFORM", None))
        )
        timings.ACTIVE_PLATFORM = LGA1700_DDR4

    # dimm_inventory caches its decode process-wide, so a real reading taken by
    # an earlier test module would otherwise leak into these rows.
    sys.modules.pop("dimm_inventory", None)
    sys.modules.pop("intel_timings", None)
    return importlib.import_module("intel_timings")


def restore():
    """Put the real modules back so later test modules get the real backend."""
    while _SAVED_ACTIVE_PLATFORM:
        timings, platform = _SAVED_ACTIVE_PLATFORM.pop()
        timings.ACTIVE_PLATFORM = platform
    sys.modules.pop("intel_timings", None)
    for name, module in _SAVED_MODULES.items():
        if module is None:
            sys.modules.pop(name, None)
        else:
            sys.modules[name] = module
    _SAVED_MODULES.clear()
