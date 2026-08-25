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

from rochviewer.platform_profiles import (
    LGA1700_DDR4,
    LGA1700_DDR5,
    LGA1851,
)

# The Intel platforms whose tables carry DDR5 row names.
DDR5_PLATFORMS = (LGA1700_DDR5, LGA1851)

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


# What each platform's fixture says it is. Kept together because a stub that
# claims one generation in ACTIVE_PLATFORM and another in SMBIOSMemoryType is
# the failure this module's own comment warns about: every reading agrees
# except the one that decides how the table is built.
DDR4_MEMORY = {
    "smbios_type": 26,
    "part_number": "F4-3600C14-16GVKA",
    "speed": 3600,
    "board": "PRO Z790-P WIFI DDR4 (MS-7E06)",
}
DDR5_MEMORY = {
    "smbios_type": 34,
    "part_number": "F5-8000J3848H16G",
    "speed": 8000,
    "board": "PRO Z790-P WIFI (MS-7E06)",
}


def memory_facts(platform):
    """The fixture's memory description for a platform."""
    return DDR5_MEMORY if platform in DDR5_PLATFORMS else DDR4_MEMORY


class _FakeMemoryModule:
    def __init__(self, tag, device_locator, facts=None):
        facts = facts or DDR4_MEMORY
        self.Tag = tag
        # The board's own socket name, which is what slot labels come from.
        # The reference target reports its two modules as records 1 and 3 while
        # naming them DIMMA2 and DIMMB2, so the stub says the same.
        self.DeviceLocator = device_locator
        self.BankLabel = "BANK 0"
        self.PartNumber = facts["part_number"]
        self.Manufacturer = "G Skill Intl"
        self.Capacity = str(16 * 1024 ** 3)
        self.Attributes = 2          # dual rank
        self.Speed = facts["speed"]
        self.ConfiguredClockSpeed = facts["speed"]
        self.SMBIOSMemoryType = facts["smbios_type"]


class FakeWMI:
    """A populated dual-channel Z790 board, matching the beta's target.

    DDR4 by default, which is what every caller wanted until the DDR5 row
    names needed a table built for them.
    """

    def __init__(self, platform=LGA1700_DDR4):
        self.facts = memory_facts(platform)

    def Win32_PhysicalMemoryArray(self):
        return [types.SimpleNamespace(MemoryDevices=4)]

    def Win32_PhysicalMemory(self):
        return [
            _FakeMemoryModule("Physical Memory 1", "Controller0-DIMMA2",
                              self.facts),
            _FakeMemoryModule("Physical Memory 3", "Controller1-DIMMB2",
                              self.facts),
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
            Product=self.facts["board"],
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


# The modules this stub stands in for, by their real dotted names.
#
# Named here rather than spelled out at each use: sys.modules is keyed by the
# full dotted path, so a bare "read" would install a fake nothing imports and
# leave the real module in place -- the stub would appear to work and change
# nothing, which is the worst way for a fixture to fail.
READ = "rochviewer.hardware.read"
TIMINGS_MODULE = "rochviewer.intel.intel_timings"
INVENTORY = "rochviewer.memory.dimm_inventory"
DISPATCHER = "rochviewer.timings"


def install(platform=LGA1700_DDR4):
    """Stub the hardware modules and return a freshly imported intel_timings.

    ``platform`` decides which generation the whole fixture describes -- the
    dispatcher global, the SMBIOS memory type, the part numbers and the board
    name together. It defaulted to DDR4 with no way to ask for anything else,
    which left the DDR5 row-name tests reading a DDR4 table: one skipped on
    every machine, and the other only ever took its DDR4 branch. The rename
    they exist to guard was unguarded.
    """
    for name in (READ, "wmi", TIMINGS_MODULE, INVENTORY):
        _SAVED_MODULES[name] = sys.modules.get(name)

    read_stub = types.ModuleType(READ)
    read_stub.read_timing = read_timing
    read_stub.read_physical_memory_int = lambda phys_addr, size=4: 0
    sys.modules[READ] = read_stub

    wmi_stub = types.ModuleType("wmi")
    wmi_stub.WMI = lambda *args, **kwargs: FakeWMI(platform)
    sys.modules["wmi"] = wmi_stub

    # active_platform() prefers whatever timings has already resolved over
    # re-probing WMI, so leaving that global alone would have the backend
    # describing the real host while every other reading came from the fake
    # WMI above. Stub it to match what this stub describes.
    timings = sys.modules.get(DISPATCHER)
    if timings is not None:
        _SAVED_ACTIVE_PLATFORM.append(
            (timings, getattr(timings, "ACTIVE_PLATFORM", None))
        )
        timings.ACTIVE_PLATFORM = platform

    # dimm_inventory caches its decode process-wide, so a real reading taken by
    # an earlier test module would otherwise leak into these rows.
    sys.modules.pop(INVENTORY, None)
    sys.modules.pop(TIMINGS_MODULE, None)
    return importlib.import_module(TIMINGS_MODULE)


def restore():
    """Put the real modules back so later test modules get the real backend."""
    while _SAVED_ACTIVE_PLATFORM:
        timings, platform = _SAVED_ACTIVE_PLATFORM.pop()
        timings.ACTIVE_PLATFORM = platform
    sys.modules.pop(TIMINGS_MODULE, None)
    for name, module in _SAVED_MODULES.items():
        if module is None:
            sys.modules.pop(name, None)
        else:
            sys.modules[name] = module
    _SAVED_MODULES.clear()
