"""What can be read about an AMD Radeon, and what cannot.

Four sources, none of them a vendor SDK: the PNP device id Windows already
holds, which carries the four PCI identifiers; the display class registry key,
which carries the adapter string and the memory size; the card's own PCIe
Resizable BAR capability, read out of configuration space; and a table below
that turns a device id into the names AMD publishes for that silicon.

The table is a decode table, not a transcript. "Navi 48" is a name for the
code 0x7550 read from the card, the same way "SK hynix" is a name for a JEDEC
vendor code. What it is *not* is a substitute for reading: an unlisted device
gets no rows at all rather than a neighbouring part's numbers, because a table
that guesses is one that is wrong quietly.

Memory vendor and type come from a fifth source, AMD's own driver library --
see amd_adl, and see the Intel profile doing the same thing through NVAPI.
Nothing readable as a register carries the vendor: the expansion ROM BAR is
unassigned so there is no VBIOS to parse, MC_SEQ_MISC0 answers all-ones on
RDNA4, and the driver's registry subtree has no vendor string in it. The
library has it because the driver read the VBIOS at load time.
"""

from __future__ import annotations

import winreg

AMD_VENDOR_ID = 0x1002

DISPLAY_CLASS_KEY = (
    r"SYSTEM\CurrentControlSet\Control\Class"
    r"\{4d36e968-e325-11ce-bfc1-08002be10318}"
)

# Per (device id, revision): the silicon's name, its process node, and the
# shader, ROP and TM unit counts AMD publishes for the part.
#
# Keyed on the revision as well as the device, because one device id covers a
# family: 0x7550 is Navi 48, and the cut a board carries is what decides the
# unit counts. Only the pair measured on this bench is listed. Every other
# card reads an em dash, which is the honest answer until one is in front of
# the tool.
#
# Bench card: PowerColor Radeon RX 9070 XT, DEV_7550 REV_C0, reporting
# 17,095,983,104 bytes of memory through the display class key.
GPU_SILICON = {
    (0x7550, 0xC0): {
        "code_name": "Navi 48",
        "technology": "4 nm",
        "cores": 4096,
        "rops": 128,
        "tmus": 256,
        "memory_type": "GDDR6",
        "bus_width": "256 bit",
    },
}

# Below this the BAR is the 256 MB legacy window every card has had since
# PCI. Anything larger means the firmware sized it to the card's memory,
# which is what Resizable BAR being on means.
LEGACY_BAR_MEGABYTES = 256


def _registry_adapter():
    """Return the display class key's values for the first real adapter."""
    try:
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE,
                            DISPLAY_CLASS_KEY) as parent:
            for index in range(16):
                try:
                    name = winreg.EnumKey(parent, index)
                except OSError:
                    break
                if not name.isdigit():
                    continue
                with winreg.OpenKey(parent, name) as key:
                    values = {}
                    for slot in range(winreg.QueryInfoKey(key)[1]):
                        value_name, value, _kind = winreg.EnumValue(key, slot)
                        values[value_name] = value
                    if values.get("HardwareInformation.qwMemorySize"):
                        return values
    except Exception:
        pass
    return {}


def memory_size_text(byte_count):
    """Render the adapter's memory the way a card is sold.

    Rounded to whole gigabytes because the firmware carves a little off what
    it reports: this card answers 17,095,983,104 bytes, which is 80 MiB under
    a round 16 GiB. Printing 15.92 would be precise about the wrong thing.
    """
    if not byte_count:
        return None
    return "%d GB" % round(int(byte_count) / (1024 ** 3))


def resizable_bar_text(megabytes):
    """Render the BAR size as the verdict plus the reading behind it."""
    if not megabytes:
        return None
    state = "Enabled" if megabytes > LEGACY_BAR_MEGABYTES else "Disabled"
    if megabytes >= 1024 and megabytes % 1024 == 0:
        return "%s (%d GB)" % (state, megabytes // 1024)
    return "%s (%d MB)" % (state, megabytes)


def driver_date_text(raw):
    """WMI datetime yyyymmddHHMMSS -> yyyy-mm-dd, as the BIOS date reads."""
    raw = str(raw or "")
    if len(raw) < 8 or not raw[:8].isdigit():
        return None
    return "%s-%s-%s" % (raw[0:4], raw[4:6], raw[6:8])


def read_gpu(pnp_device_ids=None):
    """Return what is known about the installed Radeon, or {}.

    Every key is optional. A field that could not be read is absent rather
    than blank, so the caller shows an em dash rather than a zero.
    """
    from rochviewer.system_identity import adapter_identity, board_vendor

    identity = adapter_identity(pnp_device_ids)
    if not identity or identity.get("vendor_id") != AMD_VENDOR_ID:
        return {}

    found = {
        "device_id": identity["device_id"],
        "revision": identity["revision"],
        # GPU-Z spells the revision as the bare hex byte the card reports.
        "revision_text": "%02X" % identity["revision"],
    }
    vendor = board_vendor(identity.get("subsystem_vendor_id"))
    if vendor:
        found["board_manufacturer"] = vendor

    registry = _registry_adapter()
    name = registry.get("HardwareInformation.AdapterString")
    if isinstance(name, bytes):
        name = name.decode("utf-16-le", "ignore").strip("\x00")
    if name:
        found["name"] = str(name).strip()
    memory = memory_size_text(registry.get("HardwareInformation.qwMemorySize"))
    if memory:
        found["memory_size"] = memory

    silicon = GPU_SILICON.get((identity["device_id"], identity["revision"]))
    if silicon:
        found.update(silicon)

    from rochviewer.system_identity import (
        find_display_function, resizable_bar_megabytes,
    )

    bar = resizable_bar_text(
        resizable_bar_megabytes(find_display_function(AMD_VENDOR_ID))
    )
    if bar:
        found["resizable_bar"] = bar

    driver = _driver_details()
    found.update(driver)

    # Last, so the driver's own answers win over the table's. It reports the
    # memory type as well, which is the same GDDR6 the table claims -- read
    # rather than looked up is better on principle, and it means a card the
    # table does not know still gets a type.
    from rochviewer.amd.adl import read_memory

    location = find_display_function(AMD_VENDOR_ID)
    found.update(read_memory(*location) if location else read_memory())
    return found


def _driver_details():
    """The installed driver's version and release date, from WMI."""
    try:
        import wmi

        for adapter in wmi.WMI().Win32_VideoController():
            if "Basic Display" in (getattr(adapter, "Name", "") or ""):
                continue
            found = {}
            version = (getattr(adapter, "DriverVersion", "") or "").strip()
            if version:
                found["driver_version"] = version
            date = driver_date_text(getattr(adapter, "DriverDate", ""))
            if date:
                found["driver_date"] = date
            return found
    except Exception:
        pass
    return {}


def rops_tmus_text(reader=read_gpu):
    """ROP and TM unit counts as one row, the way GPU-Z pairs them."""
    found = reader() or {}
    rops, tmus = found.get("rops"), found.get("tmus")
    if rops is None or tmus is None:
        return None
    return "%d / %d" % (rops, tmus)
