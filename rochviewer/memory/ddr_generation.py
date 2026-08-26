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

"""Which DDR generation is installed, from what the firmware already says.

SMBIOS and WMI, with no register read anywhere -- which is why this lives
here rather than in a platform backend. Both platforms need the answer and
neither needs a memory controller to get it.

It used to live in intel_timings, and asking for it from the AM5 side meant
importing that module. Importing it runs its body, and the body builds the
whole Intel table eagerly -- about 2,560 physical reads at the hardcoded
MCHBAR window, on a board where that window belongs to the FCH instead. The
dispatcher promises that cannot happen; this is what makes the promise true.

The WMI cache moved with it so there is still exactly one connection for the
process. Opening one is most of what a lookup costs, and splitting the cache
in two would have paid that twice to avoid paying it once.
"""

from functools import lru_cache

import wmi

# SMBIOS memory-type codes, from the spec's table 7.18.2: DDR4 with its
# low-power form, then DDR5 with its own.
_SMBIOS_DDR4 = (26, 30)
_SMBIOS_DDR5 = (34, 35)


@lru_cache(maxsize=None)
def _wmi_connection():
    """One WMI connection for the process.

    Building it is most of what a WMI lookup costs, and the timing table
    opened thirteen of them while it was being built.
    """
    return wmi.WMI()


@lru_cache(maxsize=None)
def _wmi_static(class_name):
    """Return a WMI class's objects, queried once.

    Only for classes describing identity or installed hardware, which cannot
    change while the machine is running. Nine such classes were being queried
    eighteen times between them.

    Failure is cached too, deliberately: WMI being unavailable is not a
    transient condition, and retrying it once per caller is how the startup
    cost multiplied in the first place. dimm_inventory caches its own decode
    the same way.
    """
    try:
        return tuple(getattr(_wmi_connection(), class_name)())
    except Exception as e:
        print(f"Error querying {class_name}: {e}")
        return ()


def detect_ddr_generation(wmi_static=None):
    """Return DDR4, DDR5, or Unknown using SMBIOS/WMI with board-name fallbacks.

    ``wmi_static`` is the accessor to ask, defaulting to this module's cached
    one. The Intel table passes its own so that patching the accessor there
    still steers this, which is the seam its tests have always used.
    """
    _wmi_static = globals()["_wmi_static"] if wmi_static is None else wmi_static
    try:
        detected = []
        for memory in _wmi_static("Win32_PhysicalMemory"):
            for field in ("SMBIOSMemoryType", "MemoryType"):
                raw = getattr(memory, field, None)
                try:
                    code = int(raw)
                except (TypeError, ValueError):
                    continue
                if code in _SMBIOS_DDR4:
                    detected.append("DDR4")
                elif code in _SMBIOS_DDR5:
                    detected.append("DDR5")

        if "DDR5" in detected:
            return "DDR5"
        if "DDR4" in detected:
            return "DDR4"

        # Some systems do not populate SMBIOSMemoryType correctly. Check text fields.
        text_fields = []
        for memory in _wmi_static("Win32_PhysicalMemory"):
            for field in ("PartNumber", "Description", "Caption"):
                value = (getattr(memory, field, "") or "").upper()
                if value:
                    text_fields.append(value)
        for board in _wmi_static("Win32_BaseBoard"):
            text_fields.extend([
                (getattr(board, "Product", "") or "").upper(),
                (getattr(board, "Version", "") or "").upper(),
            ])
        joined = " ".join(text_fields)
        if "DDR5" in joined:
            return "DDR5"
        if "DDR4" in joined:
            return "DDR4"
    except Exception as e:
        print(f"Error detecting DDR generation: {e}")
    return "Unknown"
