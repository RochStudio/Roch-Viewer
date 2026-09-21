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

"""DDR4 SPD identity, read from the module's EE1004 EEPROM.

SMBIOS carries no build date and may omit the serial and DRAM vendor, so these
rows need the module's own SPD. On DDR5 that is :mod:`ddr5_spd`; this is the
DDR4 half.

Nothing here can write EEPROM data. DDR4 page selection does require the
standard volatile EE1004 Send Byte command at fixed SPA0/SPA1 addresses; the
transport exposes that selector directly and never accepts an EEPROM write.

A DDR4 module's SPD is 512 bytes exposed 256 at a time, and the manufacturing
block -- bytes 320 to 352 -- is in the upper half. The reader selects page 1,
reads the identity bytes while holding the shared SMBus mutex, then restores
page 0 for the next tool.

The proof is the part number. Bytes 329-348 are twenty ASCII characters, and
on the Z790-P bench registers 0x49-0x5C read "F4-3600C14-16GVKA" -- which is
the part number SMBIOS reports for the same stick, and which lands at that
offset only if the window base is byte 256. The JEP106 pairs on either side
agree: 0x04CD at register 0x40 is G.Skill, the module maker, and 0x80CE at
0x5E is Samsung, the DRAM maker. Three fields from three different parts of
the block, all landing where the upper half puts them.

If that check fails because the bus is unreadable, this reports nothing and
the rows fall back to SMBIOS. It never decodes a block it has not first
confirmed from the part number.

Optional bytes may be unprogrammed even when the read succeeded. The original
G.Skill bench kit has zeros across that optional block. The Acer
BL.9BWWR.298 kit used for the 1.0.5 fix supplies week 10 of 2022 and Samsung
stepping 0x42, while its serial is zero; firmware and the reference display it
state as ``0``.
"""

from rochviewer.memory.ddr5_spd import EM_DASH, decode_jep106_id, decode_manufacture_date

SPD_MODULE_MFG_ID = 0x40          # byte 320, 2 bytes, JEP106
SPD_MFG_LOCATION = 0x42           # byte 322
SPD_MFG_YEAR = 0x43               # byte 323, BCD, years since 2000
SPD_MFG_WEEK = 0x44               # byte 324, BCD, ISO week
SPD_SERIAL_NUMBER = 0x45          # byte 325, 4 bytes
SPD_SERIAL_LENGTH = 4
SPD_PART_NUMBER = 0x49            # byte 329, 20 ASCII bytes
SPD_PART_NUMBER_LENGTH = 20
SPD_MODULE_REVISION = 0x5D        # byte 349
SPD_DRAM_MFG_ID = 0x5E            # byte 350, 2 bytes, JEP106
SPD_DRAM_STEPPING = 0x60          # byte 352

# The lower half puts the DRAM device type here. Reading it confirms the
# negative case as directly as the part number confirms the positive one:
# 0x0C at register 0x02 means the window is on the lower half and the
# manufacturing block is not reachable without a write.
SPD_DEVICE_TYPE = 0x02
SPD_DEVICE_TYPE_DDR4 = 0x0C

# A part number is the alignment proof, so it has to look like one: printable,
# and long enough that a run of stray bytes cannot pass for it.
MINIMUM_PART_NUMBER = 4

# Which hubs answered last, so repeat reads skip the bus scan.
_CACHE = []
_MODULE_CACHE = []


def decode_part_number(values):
    """The twenty ASCII bytes, or "" when they are not ASCII at all."""
    characters = []
    for offset in range(SPD_PART_NUMBER,
                        SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH):
        byte = values.get(offset)
        if byte is None:
            continue
        byte = int(byte) & 0xFF
        # Unprogrammed tail bytes are 0x00 or 0xFF and end the string rather
        # than corrupting it; anything else non-printable means this is not a
        # part number and the caller should not trust the block.
        if byte in (0x00, 0xFF):
            continue
        if not 0x20 <= byte < 0x7F:
            return ""
        characters.append(chr(byte))
    return "".join(characters).strip()


def decode_serial_number(values):
    """The serial as eight hex digits, ``0``, or an em dash.

    Some DDR4 modules leave all four bytes zero. Firmware and the reference expose that
    state as serial ``0``, so preserve it rather than making a successful SPD
    read look unavailable. An all-FF block is still an erased/unreadable value.
    """
    digits = ""
    for offset in range(SPD_SERIAL_NUMBER,
                        SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH):
        byte = values.get(offset)
        if byte is None:
            return EM_DASH
        digits += "%02X" % (int(byte) & 0xFF)
    if not digits.strip("0"):
        return "0"
    if not digits.strip("F"):
        return EM_DASH
    return digits


def decode_identity(values):
    """Decode one manufacturing block, or None when it is not one.

    None means the alignment check failed, which is the only thing standing
    between this and decoding the lower half of the SPD as though it were the
    upper one.
    """
    part_number = decode_part_number(values)
    if len(part_number) < MINIMUM_PART_NUMBER:
        return None
    return {
        "part_number": part_number,
        "module_manufacturer": decode_jep106_id(
            values.get(SPD_MODULE_MFG_ID), values.get(SPD_MODULE_MFG_ID + 1)
        ),
        "serial_number": decode_serial_number(values),
        "manufacture_date": decode_manufacture_date(
            values.get(SPD_MFG_YEAR), values.get(SPD_MFG_WEEK)
        ),
        "dram_manufacturer": decode_jep106_id(
            values.get(SPD_DRAM_MFG_ID), values.get(SPD_DRAM_MFG_ID + 1)
        ),
        "dram_stepping": values.get(SPD_DRAM_STEPPING),
    }


# Registers needed from DDR4's upper manufacturing page. The transport reads
# their contiguous range after selecting EE1004 page 1.
IDENTITY_REGISTERS = (
    tuple(range(SPD_MODULE_MFG_ID, SPD_MODULE_MFG_ID + 2))
    + (SPD_MFG_LOCATION, SPD_MFG_YEAR, SPD_MFG_WEEK)
    + tuple(range(SPD_SERIAL_NUMBER, SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH))
    + tuple(range(SPD_PART_NUMBER, SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH))
    + (SPD_MODULE_REVISION,)
    + tuple(range(SPD_DRAM_MFG_ID, SPD_DRAM_MFG_ID + 2))
    + (SPD_DRAM_STEPPING,)
)


def read_identity(reader_factory=None, refresh=False):
    """Return one identity dict per DDR4 module that answers, or [].

    Cached like its DDR5 counterpart: the System Info rows read it at startup
    and the bus scan is the slow part.
    """
    use_cache = reader_factory is None
    if use_cache and _CACHE and not refresh:
        return _CACHE[0]

    modules = []
    try:
        from rochviewer.memory.ddr5_telemetry import default_smbus_backend

        backend = default_smbus_backend()
        if backend is None and reader_factory is None:
            return []
        if backend is None:
            # See the note in ddr5_spd.read_identity: a caller that supplied
            # a reader supplied the transport, and the addresses are JEDEC's
            # rather than the platform's.
            from rochviewer.intel.intel_pch_smbus import (
                CONTROLLER_OFFSETS as controllers,
                SPD_HUB_ADDRESSES as hub_addresses,
            )

            default_factory = None
        else:
            default_factory, controllers, hub_addresses, _pmics = backend
        reader = (reader_factory or default_factory)()
        if reader.is_driver_open():
            for controller in controllers:
                for address in hub_addresses:
                    identity = _read_one(reader, address, controller)
                    if identity is not None:
                        modules.append(identity)
                # Modules live on one controller; once it answered, stop
                # probing the other, since probing an empty address blocks.
                if modules:
                    break
    except Exception as exc:
        print(f"Error reading DDR4 SPD identity: {exc}")
        modules = []

    if use_cache:
        _CACHE[:] = [modules]
    return modules


def _read_one(reader, address, controller):
    """One module's identity, or None when this address has nothing to say."""
    try:
        if not reader.probe_address(address, controller):
            return None
        start = 0x100 + min(IDENTITY_REGISTERS)
        end = 0x100 + max(IDENTITY_REGISTERS) + 1
        absolute = reader.read_ddr4_spd(
            address, start, end - start, controller
        )
        values = {
            position - 0x100: value
            for position, value in absolute.items()
        }
    except (OSError, TimeoutError, ValueError):
        return None
    identity = decode_identity(values)
    if identity is None:
        return None
    identity.update(address=address, controller=controller)
    return identity


def read_modules(reader_factory=None, refresh=False):
    """Return complete, decoded DDR4 SPD records for the SPD tab.

    The base timing section and XMP user area are read through the existing
    EE1004 page-aware transport.  No EEPROM data is written; only the volatile
    page selector used by every DDR4 SPD reader changes while a page is read.
    """
    use_cache = reader_factory is None
    if use_cache and _MODULE_CACHE and not refresh:
        return _MODULE_CACHE[0]

    modules = []
    try:
        from rochviewer.memory.ddr5_telemetry import default_smbus_backend
        from rochviewer.memory.spd_profiles import decode_ddr4_spd

        backend = default_smbus_backend()
        if backend is None and reader_factory is None:
            return []
        if backend is None:
            from rochviewer.intel.intel_pch_smbus import (
                CONTROLLER_OFFSETS as controllers,
                SPD_HUB_ADDRESSES as hub_addresses,
            )
            default_factory = None
        else:
            default_factory, controllers, hub_addresses, _pmics = backend
        reader = (reader_factory or default_factory)()
        if reader.is_driver_open():
            identities = {
                (item.get("controller"), item.get("address")): item
                for item in read_identity(reader_factory=reader_factory,
                                          refresh=refresh)
            }
            for controller in controllers:
                for address in hub_addresses:
                    identity = identities.get((controller, address))
                    if identity is None:
                        continue
                    values = {}
                    # Read only bytes the display decodes. A Byte Data SMBus
                    # transaction is issued for every position, so skipping
                    # the reserved areas cuts the first-open wait sharply.
                    for start, length in ((2, 28), (120, 6), (384, 6)):
                        values.update(reader.read_ddr4_spd(
                            address, start, length, controller
                        ))
                    enabled = int(values.get(386, 0) or 0)
                    profile_ranges = (
                        ((393, 14), (427, 5)),
                        ((440, 14), (474, 5)),
                    )
                    for index, ranges in enumerate(profile_ranges):
                        if not (enabled & (1 << index)):
                            continue
                        for start, length in ranges:
                            values.update(reader.read_ddr4_spd(
                                address, start, length, controller
                            ))
                    decoded = decode_ddr4_spd(values, identity)
                    if decoded is not None:
                        decoded.update(address=address, controller=controller)
                        modules.append(decoded)
                if modules:
                    break
    except Exception as exc:
        print(f"Error reading DDR4 SPD profiles: {exc}")
        modules = []

    if use_cache:
        _MODULE_CACHE[:] = [modules]
    return modules
