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

"""Early platform classification for Roch Viewer.

This module intentionally performs no privileged hardware access.  The result
is selected before importing a platform timing backend, so AMD systems can
never fall through to Intel MCHBAR reads.
"""

import re

LGA1700_DDR4 = "lga1700-ddr4"
LGA1700_DDR5 = "lga1700-ddr5"
LGA1851 = "lga1851"
AM5 = "am5"
UNSUPPORTED = "unsupported"

DDR4_SMBIOS_TYPE = 26
DDR5_SMBIOS_TYPE = 34


def _is_desktop_am5_cpu(cpu_name):
    name = str(cpu_name or "").strip().lower()
    if "ryzen" not in name or "threadripper" in name:
        return False
    match = re.search(
        r"ryzen\s+(?:[3579]\s+)?(?:pro\s+)?(\d{4})([a-z0-9]*)",
        name,
    )
    if not match:
        return False
    model = match.group(1)
    suffix = match.group(2)
    if model[0] not in "79":
        return False
    # Ryzen 8000 desktop parts are Phoenix APUs (including 8700F) and do not
    # inherit the validated desktop-IOD UMC profile. G/GE parts in any later
    # family remain unsupported until their layout is independently verified.
    # Mobile HS/HX/H/U parts likewise use different layouts.
    return suffix in {"", "x", "x3d", "f", "xt"}


def is_granite_ridge_cpu(cpu_name):
    """Return whether the non-privileged CPU name identifies desktop Ryzen 9000."""
    name = str(cpu_name or "").strip().lower()
    if not _is_desktop_am5_cpu(name):
        return False
    match = re.search(
        r"ryzen\s+(?:[3579]\s+)?(?:pro\s+)?(\d{4})([a-z0-9]*)",
        name,
    )
    return bool(match and match.group(1).startswith("9"))


def _is_lga1700_desktop_cpu(cpu_name):
    name = str(cpu_name or "").strip().lower()
    match = re.search(
        r"core(?:\(tm\))?\s+i[3579]-?(\d{5})([a-z]*)\b", name
    )
    if not match or match.group(1)[:2] not in {"12", "13", "14"}:
        return False
    return match.group(2) in {"", "f", "k", "kf", "ks", "t"}


def _is_lga1851_desktop_cpu(cpu_name):
    name = str(cpu_name or "").strip().lower()
    match = re.search(
        r"core(?:\(tm\))?\s+ultra\s+[579]\s+(\d{3})([a-z]*)\b",
        name,
    )
    if not match or not match.group(1).startswith("2"):
        return False
    return match.group(2) in {"", "f", "k", "kf", "ks", "t"}


def classify_platform(
    manufacturer, cpu_name, memory_types, board_product=""
):
    """Classify from non-privileged WMI/SMBIOS strings and memory types."""
    vendor = str(manufacturer or "").strip().lower()
    name = str(cpu_name or "").strip().lower()
    board = str(board_product or "").strip().lower()
    try:
        types = {int(value) for value in memory_types if value is not None}
    except (TypeError, ValueError):
        types = set()

    is_amd = "authenticamd" in vendor or vendor == "amd" or "advanced micro devices" in vendor
    is_intel = "genuineintel" in vendor or "intel" in vendor

    if is_amd:
        # Desktop AM5 is DDR5-only. Requiring both Ryzen identity and DDR5
        # prevents an AM4 system from ever reaching the AM5 SMN decoder.
        if _is_desktop_am5_cpu(name) and DDR5_SMBIOS_TYPE in types:
            return AM5
        return UNSUPPORTED

    if is_intel:
        if DDR5_SMBIOS_TYPE in types and _is_lga1851_desktop_cpu(name):
            return LGA1851
        if DDR4_SMBIOS_TYPE in types and _is_lga1700_desktop_cpu(name):
            return LGA1700_DDR4
        if DDR5_SMBIOS_TYPE in types and _is_lga1700_desktop_cpu(name):
            return LGA1700_DDR5

    return UNSUPPORTED


def detect_current_platform(wmi_factory=None):
    """Return the current platform ID using only WMI/SMBIOS metadata."""
    try:
        if wmi_factory is None:
            import wmi

            wmi_factory = wmi.WMI
        connection = wmi_factory()
        processors = connection.Win32_Processor()
        memories = connection.Win32_PhysicalMemory()
        boards = connection.Win32_BaseBoard()
        processor = processors[0] if processors else None
        board = boards[0] if boards else None
        return classify_platform(
            getattr(processor, "Manufacturer", ""),
            getattr(processor, "Name", ""),
            tuple(
                getattr(memory, "SMBIOSMemoryType", None)
                or getattr(memory, "MemoryType", None)
                for memory in memories
            ),
            getattr(board, "Product", ""),
        )
    except Exception:
        return UNSUPPORTED
