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

"""Identity readings that are the same question on any platform.

The memory generation, the microcode revision and the Super I/O on the LPC
bus are facts about the machine rather than about its memory controller, and
they are read the same way whoever made the CPU: SMBIOS, the registry, and
the chip's own ID port. They live here so both platform profiles can ask for
them, rather than one of them owning readings the other also needs.

The reads live here; the tables do not. Naming silicon from a device ID is a
per-platform claim -- Intel's host bridge table answers on an AMD board too,
and would name 0x14D8 as an Intel part -- so each profile keeps its own table
and this module only supplies the reading.
"""

from __future__ import annotations

import re
import winreg

# SMBIOS Type 17 memory-type codes, which are numbers rather than names in
# the firmware. Only the generations this tool runs on are decoded; anything
# else is reported as its code so an unknown board still says something true.
SMBIOS_MEMORY_TYPES = {
    0x18: "DDR3",
    0x1A: "DDR4",
    0x22: "DDR5",
    0x23: "LPDDR5",
}

MICROCODE_REGISTRY_PATH = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"

# Which vendor each Super I/O reader speaks for. The chip names the readers
# carry -- NCT6687D, NCT6798D, IT8696E -- do not say who makes them, and the
# row is read as "LPCIO" the way a board's spec sheet writes it.
LPCIO_VENDORS = {
    "superio_lpc": "Nuvoton",
    "nct679x": "Nuvoton",
    "ite_superio": "ITE",
}


WINDOWS_VERSION_KEY = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"

# Windows' PNP device id carries the four PCI fields already, so a display
# adapter can be identified without walking configuration space.
_PNP_IDENTITY = re.compile(
    r"VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})&SUBSYS_([0-9A-F]{4})([0-9A-F]{4})"
    r"&REV_([0-9A-F]{2})",
    re.IGNORECASE,
)

# PCI subsystem vendors: who assembled the board, whoever made the chip on it.
# 0x148C is measured on this bench, where the card is a PowerColor; the rest
# are the other major add-in-board vendors. An unlisted vendor prints its ID
# rather than a guess.
BOARD_VENDORS = {
    0x1002: "AMD",
    0x1043: "ASUSTeK Computer",
    0x1458: "GIGABYTE Technology",
    0x1462: "MSI",
    0x148C: "PowerColor",
    0x1569: "Palit Microsystems",
    0x174B: "Sapphire Technology",
    0x196E: "PNY",
    0x19DA: "Zotac",
    0x1DA2: "Sapphire Technology",
    0x1EAE: "XFX",
    0x3842: "EVGA",
    0x10DE: "NVIDIA",
}


def windows_version_values():
    """The version key's values, read in one open. None of them can change."""
    found = {}
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                            WINDOWS_VERSION_KEY) as key:
            for name in ("UBR", "DisplayVersion", "ReleaseId"):
                try:
                    found[name], _kind = winreg.QueryValueEx(key, name)
                except OSError:
                    continue
    except Exception:
        return {}
    return found


def os_name():
    """Windows edition, architecture and full build, as CPU-Z states it.

    Its About tab reads "Microsoft Windows 11 Professional (x64) 23H2 Build
    22631.6199", which is the same facts WMI carries plus the update revision
    -- so the parts are assembled rather than the caption printed as-is.
    "Pro" is spelled the way the edition is: WMI abbreviates it in the caption
    while the edition itself is Professional.
    """
    try:
        import wmi

        systems = wmi.WMI().Win32_OperatingSystem()
        if not systems:
            return None
        caption = (getattr(systems[0], "Caption", "") or "").strip()
        if not caption:
            return None
        if not caption.startswith("Microsoft "):
            caption = "Microsoft " + caption
        for short, full in (("Pro", "Professional"), ("Ent", "Enterprise")):
            if caption.endswith(" " + short):
                caption = caption[: -len(short)] + full
        architecture = (
            getattr(systems[0], "OSArchitecture", "") or ""
        ).strip()
        if architecture:
            # WMI says "64-bit"; CPU-Z, and everyone else, says x64.
            caption += " (x64)" if "64" in architecture else " (x86)"
        values = windows_version_values()
        # winver's ordering: the feature update, then the build it produced.
        display_version = values.get("DisplayVersion") or values.get("ReleaseId")
        if display_version:
            caption += " " + str(display_version)
        build = (getattr(systems[0], "BuildNumber", "") or "").strip()
        if build:
            caption += " Build %s" % build
            revision = values.get("UBR")
            if isinstance(revision, int):
                caption += ".%d" % revision
        return caption
    except Exception:
        return None


def adapter_identity(pnp_device_ids=None):
    """Return the display adapter's PCI identity, or None.

    From the PNP device id Windows already holds, which carries vendor,
    device, subsystem and revision. Walking configuration space for the same
    four cost a map/unmap pair per dword across 64 functions and returned the
    first VGA-class device it found -- the integrated graphics, on a machine
    that has both.
    """
    try:
        if pnp_device_ids is None:
            import wmi

            pnp_device_ids = [
                str(getattr(adapter, "PNPDeviceID", "") or "")
                for adapter in wmi.WMI().Win32_VideoController()
                if "Basic Display" not in (getattr(adapter, "Name", "") or "")
            ]
        for pnp_device_id in pnp_device_ids:
            found = _PNP_IDENTITY.search(str(pnp_device_id or ""))
            if not found:
                continue
            return {
                "vendor_id": int(found.group(1), 16),
                "device_id": int(found.group(2), 16),
                # SUBSYS is device then vendor, high half first.
                "subsystem_vendor_id": int(found.group(4), 16),
                "subsystem_device_id": int(found.group(3), 16),
                "revision": int(found.group(5), 16),
            }
    except Exception:
        pass
    return None


def board_vendor(subsystem_vendor_id):
    """Who assembled the board, or its ID when the table does not name it."""
    if subsystem_vendor_id is None:
        return None
    return BOARD_VENDORS.get(subsystem_vendor_id,
                             "0x%04X" % subsystem_vendor_id)


def pci_config_dword(device, function, offset, bus=0):
    """Read one dword from a PCI function's configuration space, or None.

    Through the firmware's own ECAM window, which is neither an Intel nor an
    AMD idea -- both platforms name their silicon by reading the same two
    registers here, so the read lives once and each profile brings its own
    table of what the identifiers mean.
    """
    try:
        from pci_mcfg import ecam_address
        from intel_pch_smbus import default_ecam_allocation
        from read import read_physical_memory_int

        allocation = default_ecam_allocation()
        if allocation is None:
            return None
        return read_physical_memory_int(
            ecam_address(allocation, bus, device, function, offset), 4
        )
    except Exception:
        return None


PCI_HEADER_BRIDGE = 1
PCI_CLASS_DISPLAY = 0x03

# Where the extended capability list starts, and the Resizable BAR entry's id.
PCI_EXTENDED_CAPABILITIES = 0x100
RESIZABLE_BAR_CAPABILITY = 0x0015
MAX_CAPABILITY_HOPS = 32


def find_display_function(vendor_id, bus=0, depth=0):
    """Walk down the bridges to a display function, or None.

    The card is not on bus 0 -- here it sits three bridges down, at 03:00.0 --
    and the PNP id Windows hands out does not carry the bus. Each bridge names
    the bus behind it, so the walk follows those rather than sweeping 256
    buses for a device that is on one of them.
    """
    if depth > 6:
        return None
    for device in range(32):
        for function in range(8):
            identity = pci_config_dword(device, function, 0x00, bus)
            if not identity or identity == 0xFFFFFFFF:
                continue
            classcode = (pci_config_dword(device, function, 0x08, bus) or 0) >> 16
            if ((classcode >> 8) == PCI_CLASS_DISPLAY
                    and (identity & 0xFFFF) == vendor_id):
                return bus, device, function
            header = (
                (pci_config_dword(device, function, 0x0C, bus) or 0) >> 16
            ) & 0x7F
            if header != PCI_HEADER_BRIDGE:
                continue
            secondary = (
                (pci_config_dword(device, function, 0x18, bus) or 0) >> 8
            ) & 0xFF
            if secondary and secondary != bus:
                found = find_display_function(vendor_id, secondary, depth + 1)
                if found:
                    return found
    return None


def resizable_bar_megabytes(location):
    """The size the card's first BAR is currently programmed to, in MB.

    From the Resizable BAR capability's control register, which reports the
    size in use. Nothing is written: the usual way to size a BAR is to write
    all ones and read back the mask, and this tool does not write to PCI
    configuration space. A card without the capability returns None, which is
    not the same as a card that has it and is set to the 256 MB default.
    """
    if not location:
        return None
    bus, device, function = location
    offset = PCI_EXTENDED_CAPABILITIES
    for _hop in range(MAX_CAPABILITY_HOPS):
        header = pci_config_dword(device, function, offset, bus)
        if header is None or header in (0, 0xFFFFFFFF):
            return None
        if (header & 0xFFFF) == RESIZABLE_BAR_CAPABILITY:
            control = pci_config_dword(device, function, offset + 0x08, bus)
            if control is None:
                return None
            return 1 << ((control >> 8) & 0x3F)
        offset = (header >> 20) & 0xFFF
        if not offset:
            return None
    return None


def pci_device_and_revision(device, function):
    """Return ``(device id, revision)`` for one PCI function, or (None, None)."""
    identity = pci_config_dword(device, function, 0x00)
    if identity is None or identity in (0, 0xFFFFFFFF):
        return None, None
    revision = (pci_config_dword(device, function, 0x08) or 0) & 0xFF
    return (identity >> 16) & 0xFFFF, revision


def decode_wmi_processor_id(processor_id):
    """Decode displayed CPUID family/model from WMI ProcessorId leaf-1 EAX.

    Here rather than beside the probe that first needed it. It is pure
    arithmetic on a string, but it lived in amd_smu_version_probe -- a
    write-capable research tool that must never ship -- and one import of it
    from intel_timings was enough to pull that whole module into the release
    build. A shared decoder belongs somewhere shippable.
    """
    text = str(processor_id or "").strip()
    if len(text) < 8:
        raise ValueError("WMI ProcessorId is unavailable")
    try:
        eax = int(text[-8:], 16)
    except ValueError as exc:
        raise ValueError("WMI ProcessorId is invalid") from exc
    base_family = (eax >> 8) & 0xF
    extended_family = (eax >> 20) & 0xFF
    family = base_family + extended_family if base_family == 0xF else base_family
    base_model = (eax >> 4) & 0xF
    extended_model = (eax >> 16) & 0xF
    model = (
        base_model | (extended_model << 4)
        if base_family in (0x6, 0xF)
        else base_model
    )
    return family, model


def memory_type():
    """The DDR generation, as SMBIOS states it, or None."""
    try:
        import wmi

        found = []
        for memory in wmi.WMI().Win32_PhysicalMemory():
            code = getattr(memory, "SMBIOSMemoryType", None)
            if code is None:
                continue
            name = SMBIOS_MEMORY_TYPES.get(int(code), "Type %d" % int(code))
            if name not in found:
                found.append(name)
        return " / ".join(found) if found else None
    except Exception:
        return None


def decode_microcode(value):
    """Return the revision out of the registry's Update Revision value.

    Windows commonly stores the revision in the upper 32 bits, so 0x13300000000
    means revision 0x133. Split apart rather than printed whole, which is how
    this row used to disagree with every other tool.
    """
    if isinstance(value, (bytes, bytearray)):
        revision = int.from_bytes(value, byteorder="little", signed=False)
    elif isinstance(value, int):
        revision = value
    else:
        return None
    if revision > 0xFFFFFFFF:
        upper = (revision >> 32) & 0xFFFFFFFF
        revision = upper if upper else revision & 0xFFFFFFFF
    return "0x%X" % revision


def microcode():
    """The running microcode revision, or None."""
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE, MICROCODE_REGISTRY_PATH
        ) as key:
            value, _ = winreg.QueryValueEx(key, "Update Revision")
    except Exception:
        return None
    return decode_microcode(value)


def lpcio_name():
    """The Super I/O on the LPC bus, by its chip ID, or None.

    Through the board sensors' own profile, which is the thing that already
    decided which chip is here. Probing separately would unlock and re-lock
    the configuration window under the monitoring mutex for an answer that
    cannot change. The vendor comes from the reader that answered rather than
    from the chip name, which carries no vendor of its own.
    """
    try:
        from intel_board_sensors import board_sensor_profile

        profile = board_sensor_profile()
    except Exception:
        return None
    reader = profile.get("reader") if profile else None
    name = getattr(reader, "chip_name", None)
    if not name:
        return None
    vendor = LPCIO_VENDORS.get(type(reader).__module__)
    return "%s %s" % (vendor, name) if vendor else name
