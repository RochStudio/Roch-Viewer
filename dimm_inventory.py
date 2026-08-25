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

"""Per-DIMM SMBIOS facts, shared by the bottom DIMM strip and System Info.

Both readers want the same four things about an installed module -- part
number, capacity, rank count and DRAM component -- and both used to derive
them from ``Win32_PhysicalMemory`` on their own. The query lives here once so
the two displays cannot drift apart, and so the decode can be tested without
WMI by passing a stand-in connection.
"""

import re

from dram_ic import identify_dram_ic

EM_DASH = "—"

# SMBIOS DeviceLocator is the name the board itself gives the socket. Three
# shapes cover what desktop firmware writes:
#
#   "Controller0-DIMMA2", "DIMM_A1", "DIMMB2"    channel letter then number
#   "Controller0-ChannelA-DIMM1"                 channel letter, index in it
#   "Controller0-DIMM0"                          controller index, index in it
#
# Reading it beats deriving a slot from the record's position in the query.
# Position is an SMBIOS handle order, not a slot order: on the MSI Z790-P the
# two installed modules come back as records 1 and 3 while the board calls them
# DIMMA2 and DIMMB2, so a position-based table got the slot numbers wrong and
# the channels backwards.
#
# The third shape is weaker than the other two and is tried last. It names no
# channel letter, so the letter is the controller index (0 -> A) and the number
# is the DIMM index within that controller counted from one. That is a derived
# name rather than the board's own: firmware writing this shape never states
# the silkscreen number, so on a board whose two sockets are labelled A2/B2 the
# derived name reads A1/B1. It is still tied to the record's own controller and
# DIMM index rather than its position in the query, so the channel it names is
# right and the two sticks cannot swap -- which is the failure that mattered.
# ASUS ROG MAXIMUS Z790 APEX (LGA1700 DDR5) writes this shape.
_SLOT_LETTER_NUMBER = re.compile(r"DIMM[\s_-]*([A-Za-z])(\d+)", re.IGNORECASE)
_SLOT_CHANNEL_INDEX = re.compile(
    r"CHANNEL[\s_-]*([A-Za-z]).*?DIMM[\s_-]*(\d+)", re.IGNORECASE
)
_SLOT_CONTROLLER_INDEX = re.compile(
    r"CONTROLLER[\s_-]*(\d+).*?DIMM[\s_-]*(\d+)", re.IGNORECASE
)


def parse_slot(device_locator):
    """Return the board's name for a slot, like "A2", or None when unreadable.

    None rather than a guess: a wrong slot name points a tuner at the wrong
    stick, which is worse than admitting the board did not say.
    """
    text = str(device_locator or "")
    match = _SLOT_LETTER_NUMBER.search(text)
    if match:
        return "%s%d" % (match.group(1).upper(), int(match.group(2)))
    match = _SLOT_CHANNEL_INDEX.search(text)
    if match:
        # This form numbers the DIMMs within a channel from zero.
        return "%s%d" % (match.group(1).upper(), int(match.group(2)) + 1)
    match = _SLOT_CONTROLLER_INDEX.search(text)
    if match:
        controller = int(match.group(1))
        # Past Z the letter would run into punctuation; no desktop board has
        # anywhere near that many controllers, so decline rather than invent.
        if controller < 26:
            return "%s%d" % (
                chr(ord("A") + controller), int(match.group(2)) + 1,
            )
    return None


def channel_of(slot):
    """Return the channel letter a slot name belongs to, or None."""
    return slot[0] if slot else None


def slots_by_channel(modules):
    """Return ``{channel letter: [slot names]}`` for the installed modules."""
    channels = {}
    for module in modules:
        slot = module.get("slot")
        if not slot:
            continue
        channels.setdefault(channel_of(slot), []).append(slot)
    for slots in channels.values():
        slots.sort()
    return channels

# Text fields worth handing to the IC identifier. SMBIOS usually exposes the
# module SKU but not the DRAM component marking, so anything a vendor happens
# to fill in is a chance at a real answer.
_METADATA_FIELDS = (
    "SerialNumber", "AssetTag", "DeviceLocator",
    "BankLabel", "Name", "Description", "Model", "SKU",
)

_CACHE = []


def rank_count(attributes):
    """Return the rank count from SMBIOS Type 17 Attributes, 0 when absent.

    The low nibble carries the rank count; firmware that does not fill the
    field leaves it zero.
    """
    try:
        return int(attributes or 0) & 0x0F
    except (TypeError, ValueError):
        return 0


def rank_short(count):
    """Rank as the DIMM strip writes it: SR / DR / 4R."""
    if count == 1:
        return "SR"
    if count == 2:
        return "DR"
    if count > 2:
        return f"{count}R"
    return "N/A"


def rank_numeric(count):
    """Rank as ZenTimings writes it: 1R / 2R."""
    return f"{count}R" if count > 0 else EM_DASH


def split_ic(label):
    """Split an IC label into (manufacturer, die).

    ``identify_dram_ic`` returns one string because that is what the DIMM
    strip prints, but System Info lists the maker and the die separately.

      "SK hynix A-die"        -> ("SK hynix", "A-die")
      "Micron (die unknown)"  -> ("Micron", EM_DASH)
      "SK hynix CJR"          -> ("SK hynix", "CJR")
      "Unknown IC"            -> (EM_DASH, EM_DASH)
    """
    text = (label or "").strip()
    if not text or text == "Unknown IC":
        return EM_DASH, EM_DASH
    if text.endswith("(die unknown)"):
        return text[: -len("(die unknown)")].strip() or EM_DASH, EM_DASH
    maker, _, die = text.rpartition(" ")
    if not maker:
        return text, EM_DASH
    return maker, die


def read_modules(connection=None, refresh=False):
    """Return one dict per installed module, or [] when the query fails.

    Cached: the DIMM strip and four System Info rows all read it during
    startup, and each WMI connection costs far more than the values do.
    """
    if connection is None:
        if _CACHE and not refresh:
            return _CACHE[0]
        try:
            import wmi

            connection = wmi.WMI()
        except Exception as e:
            print(f"Error retrieving memory info: {e}")
            _CACHE.clear()
            _CACHE.append([])
            return _CACHE[0]
        modules = _decode(connection)
        _CACHE.clear()
        _CACHE.append(modules)
        return modules
    return _decode(connection)


def _decode(connection):
    modules = []
    try:
        entries = connection.Win32_PhysicalMemory()
    except Exception as e:
        print(f"Error retrieving memory info: {e}")
        return modules

    for memory in entries:
        try:
            capacity_gb = int(getattr(memory, "Capacity", 0) or 0) // (1024 ** 3)
        except (TypeError, ValueError):
            capacity_gb = 0
        part_number = (getattr(memory, "PartNumber", "") or "").strip() or "Unknown"
        tag = (getattr(memory, "Tag", "") or "").strip() or "Unknown"
        module_manufacturer = (getattr(memory, "Manufacturer", "") or "").strip()
        memory_type = str(
            getattr(memory, "SMBIOSMemoryType", None)
            or getattr(memory, "MemoryType", None)
            or ""
        )

        extra_metadata = []
        for field_name in _METADATA_FIELDS:
            try:
                field_value = getattr(memory, field_name, "") or ""
            except Exception:
                field_value = ""
            if field_value:
                extra_metadata.append(str(field_value))

        ranks = rank_count(getattr(memory, "Attributes", 0))
        ic_label = identify_dram_ic(
            part_number=part_number,
            module_manufacturer=module_manufacturer,
            memory_type=memory_type,
            capacity_gb=capacity_gb,
            rank_label=rank_short(ranks),
            extra_metadata=extra_metadata,
        )

        device_locator = (
            getattr(memory, "DeviceLocator", "") or ""
        ).strip()
        slot = parse_slot(device_locator)

        # Firmware reads this off the module at POST. It is the one identity
        # field SMBIOS carries that no table can be derived from, and on DDR4
        # it is the only way to reach it: the SPD path that reads the serial
        # directly is DDR5-only. Firmware that leaves it blank, or fills it
        # with a placeholder of all zeroes or all Fs, is reporting nothing.
        serial_number = (getattr(memory, "SerialNumber", "") or "").strip()
        if not serial_number.strip("0fF"):
            serial_number = ""

        modules.append({
            "serial_number": serial_number,
            "tag": tag,
            "device_locator": device_locator,
            "slot": slot,
            "channel": channel_of(slot),
            "part_number": part_number,
            "capacity_gb": capacity_gb,
            "capacity": f"{capacity_gb}GB",
            "rank_count": ranks,
            "rank": rank_short(ranks),
            "module_manufacturer": module_manufacturer,
            "ic": ic_label,
        })
    return modules


# Boards whose firmware overstates how many DIMM sockets they have, keyed on
# the board code inside Win32_BaseBoard.Product.
#
# SMBIOS Type 16 MemoryDevices is the only readable socket total, and a board
# that shares a firmware table with a larger sibling reports the sibling's
# count: MS-7E83 (MSI B850M POWER) is a two-slot board that declares four
# devices, listing DIMMA1 and DIMMB1 as empty sockets that do not exist.
#
# Nothing can be read to settle it -- an empty socket has no SPD hub to probe,
# and the controller only knows what is populated -- so a correction is
# entered per board from the board itself. Anything not listed keeps the
# firmware count, which is right on most boards.
BOARD_MEMORY_SLOTS = {
    "MS-7E83": 2,      # MSI B850M POWER, confirmed on the bench
}

_BOARD_CODE = re.compile(r"MS-[0-9A-Z]{4,}", re.IGNORECASE)


def board_slot_count(connection):
    """Return how many DIMM sockets the board has, or 0 when unknown."""
    try:
        boards = connection.Win32_BaseBoard()
        product = (getattr(boards[0], "Product", "") or "") if boards else ""
    except Exception:
        product = ""
    match = _BOARD_CODE.search(str(product))
    if match:
        corrected = BOARD_MEMORY_SLOTS.get(match.group(0).upper())
        if corrected:
            return corrected
    try:
        arrays = connection.Win32_PhysicalMemoryArray()
        if arrays:
            return int(getattr(arrays[0], "MemoryDevices", 0) or 0)
    except Exception:
        pass
    return 0


def slots_used(connection=None):
    """Populated sockets against the board's total, e.g. "2 of 2".

    Two modules in a four-slot board behave differently from two in a
    two-slot board, so the total is the point of the row -- which is why a
    firmware count known to be wrong is corrected rather than printed. See
    BOARD_MEMORY_SLOTS.
    """
    try:
        if connection is None:
            import wmi

            connection = wmi.WMI()
        used = len(read_modules(connection))
        total = board_slot_count(connection)
        if not total or used > total:
            # A total that cannot hold the modules already counted is not a
            # total; say nothing rather than something impossible.
            return None
        return "%d of %d" % (used, total)
    except Exception as e:
        print(f"Error retrieving slot usage: {e}")
        return None


def shared_value(modules, reader):
    """Report one value for the whole set, or every distinct value.

    Mixed kits are rare but real, and printing only the first module's rank or
    die would quietly hide the mismatch that explains the timings.
    """
    values = []
    for module in modules:
        value = reader(module)
        if value and value != EM_DASH and value not in values:
            values.append(value)
    if not values:
        return EM_DASH
    return " / ".join(values)
