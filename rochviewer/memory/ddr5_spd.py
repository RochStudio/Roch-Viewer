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

"""DDR5 SPD identity: what the module itself reports about its DRAM.

SMBIOS tells us the module SKU, its capacity and its rank count, but not which
DRAM sits on it. The SPD EEPROM does: a JEP106 manufacturer ID and a die
stepping, written by the module maker rather than inferred from the part
number. This module reads those bytes over the existing read-only SPD path.

Offsets are the JESD400-5 identity block, cross-checked against ZenStates-Core
(`DRAM/DDR5/Spd/Ddr5SpdDecoder.cs`) and confirmed on the bench: reading the
part number at 0x209 returns the SKU printed on the label, which pins the whole
block's alignment, and the manufacturer at 0x228 then decodes to the DRAM maker
the same DIMM reports in ZenTimings.
"""

# JESD400-5 identity block (module-specific section, page 4).
SPD_MODULE_MFG_ID = 0x200         # 2 bytes, JEP106
SPD_MFG_YEAR = 0x203              # BCD, years since 2000
SPD_MFG_WEEK = 0x204              # BCD, ISO week
SPD_SERIAL_NUMBER = 0x205         # 4 bytes, vendor-assigned
SPD_SERIAL_LENGTH = 4
SPD_PART_NUMBER = 0x209           # 30 ASCII bytes
SPD_PART_NUMBER_LENGTH = 30
SPD_DRAM_MFG_ID = 0x228           # 2 bytes, JEP106
SPD_DRAM_STEPPING = 0x22A         # die revision, vendor-defined
SPD_IDENTITY_BLOCK = (SPD_MODULE_MFG_ID, 0x30)

EM_DASH = "—"

# JEP106 (bank, code) -> name, limited to DRAM makers, taken verbatim from
# ZenStates-Core's ManufacturerBanks. Anything absent is reported as its raw
# ID rather than guessed at.
JEP106_VENDORS = {
    (1, 0x2C): "Micron Technology",
    (1, 0xAD): "SK hynix",
    (1, 0xCE): "Samsung",
    (1, 0xDA): "Winbond Electronic",
    (3, 0xFE): "Elpida",
    (4, 0x0B): "Nanya Technology",
    (5, 0xC8): "Powerchip Semiconductor",
    (9, 0xD5): "Etron Technology Inc",
    (11, 0x91): "CXMT",
    # Module makers, for the module-vendor field rather than the DRAM one.
    # Firmware often leaves SMBIOS Manufacturer as "Unknown" while the SPD
    # carries this: 0x04CD on the bench kit, whose part number is a G.Skill
    # SKU, so the two agree.
    (5, 0xCD): "G.Skill",
    # Bank 7 code 0x6D, read as 0x866D on the LGA1700 kit. SMBIOS calls the
    # same module "V-Color Technology Inc"; this is the name the module itself
    # carries, and what CPU-Z shows against it.
    (7, 0x6D): "V-Color Technology",
}

# Which SPD hubs answered last, so repeat reads skip the bus scan.
_CACHE = []

# This block is only reachable on DDR5, and asking for it on DDR4 is worse
# than useless.
#
# The identity lives at 0x200, page 4 of the hub's EEPROM window, and the page
# has to be selected before the read. A DDR4 module has no hub and no page
# register: the same transaction reaches a plain I2C EEPROM, where the command
# byte the selector uses is not a page selector at all but an offset into the
# SPD array. So the read does not merely come back wrong -- which it did, a
# part number of ">>> '" and a die of "0xA0" on the Z790 DDR4 bench -- it aims
# a write at a module's own SPD. The PCH's SPD Write Disable stopped it
# reaching the module here, but that is a board default, not a guarantee.
#
# DDR4 identity is not reachable through this transport in any case: its bytes
# sit at 0x140-0x160 in page 1, and DDR4 selects pages by writing to the
# separate SPA0/SPA1 addresses, which this transport does not allow and which
# SPD Write Disable blocks. Callers fall back to SMBIOS, which carries the
# part number, the module maker and enough to name the DRAM.
DDR4_GENERATION = "DDR4"


def installed_generation():
    """"DDR4", "DDR5", or None when nothing here can say.

    Imported at call time: this module is imported by the one that owns the
    detection, so naming it at the top would be a cycle.
    """
    try:
        from rochviewer.intel.intel_timings import detect_ddr_generation

        return detect_ddr_generation()
    except Exception:
        return None


def decode_jep106_id(lsb, msb):
    """Return the vendor name for a JEP106 pair, or its raw ID.

    The low byte carries the continuation count in bits 6:0 (bit 7 is odd
    parity), so the bank is that count plus one; the high byte is the code.
    """
    if lsb is None or msb is None:
        return EM_DASH
    bank = (int(lsb) & 0x7F) + 1
    code = int(msb) & 0xFF
    name = JEP106_VENDORS.get((bank, code))
    if name:
        return name
    return "0x%02X%02X" % (int(lsb) & 0xFF, code)


def decode_die(manufacturer, stepping):
    """Name the die from the stepping byte, or report the byte itself.

    Only SK hynix is decoded, and only for the 0x4n family: 0x41 is A-die,
    0x42 B-die, and so on. That is the rule ZenTimings applies, and it agrees
    on this bench with the part-number table this project already carried
    (0x41 on a kit independently known to be A-die). No other vendor's
    stepping encoding is claimed here -- for those the raw byte is shown,
    which is honest about what the module actually reported.
    """
    if stepping is None:
        return EM_DASH
    stepping = int(stepping) & 0xFF
    if "hynix" in (manufacturer or "").lower():
        family, index = stepping >> 4, stepping & 0x0F
        if family == 0x4 and 1 <= index <= 15:
            return "%s-die" % chr(ord("A") + index - 1)
    return "0x%02X" % stepping


def decode_bcd(value):
    """Return a BCD byte as an int, or None if either nibble is not a digit."""
    if value is None:
        return None
    value = int(value) & 0xFF
    high, low = value >> 4, value & 0x0F
    if high > 9 or low > 9:
        return None
    return high * 10 + low


def decode_manufacture_date(year_byte, week_byte):
    """Return "WW / YYYY" for the module's build date, or an em dash.

    Both bytes are BCD, the year counted from 2000 -- the same pair CPU-Z
    shows as Week/Year on its SPD tab. Confirmed against the bench kit, which
    reads 0x25/0x04 where CPU-Z reports 04 / 25.

    An unprogrammed module leaves these at 0x00 or 0xFF, so a week outside
    1-53 is reported as nothing rather than as week 0 of 2000.
    """
    year, week = decode_bcd(year_byte), decode_bcd(week_byte)
    if year is None or week is None or not 1 <= week <= 53:
        return EM_DASH
    return "%02d / %d" % (week, 2000 + year)


def decode_serial_number(values):
    """Return the module serial as eight hex digits, or an em dash.

    Four raw bytes, printed as digits rather than converted to a decimal
    number: that is how the vendor prints it on the label and how CPU-Z shows
    it, and the two modules here read ...4996 and ...4997, consecutive.
    """
    digits = ""
    for offset in range(SPD_SERIAL_NUMBER, SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH):
        if values.get(offset) is None:
            return EM_DASH
        digits += "%02X" % (int(values[offset]) & 0xFF)
    return digits


def decode_identity(values):
    """Decode one SPD identity block, ``{offset: byte}`` as read_spd returns."""
    def byte(offset):
        return values.get(offset)

    part_number = "".join(
        chr(values[offset])
        for offset in range(
            SPD_PART_NUMBER, SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH
        )
        if offset in values and 0x20 <= values[offset] < 0x7F
    ).strip()

    manufacturer = decode_jep106_id(byte(SPD_DRAM_MFG_ID), byte(SPD_DRAM_MFG_ID + 1))
    stepping = byte(SPD_DRAM_STEPPING)
    return {
        "part_number": part_number,
        "module_manufacturer": decode_jep106_id(
            byte(SPD_MODULE_MFG_ID), byte(SPD_MODULE_MFG_ID + 1)
        ),
        "serial_number": decode_serial_number(values),
        "manufacture_date": decode_manufacture_date(
            byte(SPD_MFG_YEAR), byte(SPD_MFG_WEEK)
        ),
        "dram_manufacturer": manufacturer,
        "dram_stepping": stepping,
        "dram_die": decode_die(manufacturer, stepping),
    }


def read_identity(reader_factory=None, refresh=False, generation=None):
    """Return one identity dict per SPD hub that answers, or [].

    Cached: the System Info rows read it during startup and the bus scan is
    the slow part. An empty list means no hub answered -- no driver, no
    permissions, or a platform whose SMBus path this project has no transport
    for -- and callers are expected to fall back to what SMBIOS told them.

    DDR4 gets the same empty list without touching the bus at all. See
    DDR4_GENERATION for why running this there both misreads the module and
    points a write at its SPD.

    The transport is the platform's; see
    :func:`ddr5_telemetry.default_smbus_backend`.
    """
    if generation is None:
        generation = installed_generation()
    if generation == DDR4_GENERATION:
        return []

    # An injected factory is a test or a probe: never served from, nor written
    # to, the cache the app shares.
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
            # A caller that supplied a reader has supplied the transport; all
            # that is missing is where to point it, and that is JEDEC's, not
            # the platform's -- SPD hubs answer at 0x50-0x57 behind any host
            # controller. Without this an injected reader was discarded on a
            # machine with no recognised backend, which is exactly where a
            # test runs and exactly where the injection exists for.
            from rochviewer.intel.intel_pch_smbus import (
                CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES,
            )

            default_factory = None
        else:
            (default_factory, CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES,
             _pmics) = backend
        reader_factory = reader_factory or default_factory

        reader = reader_factory()
        if reader.is_driver_open():
            offset, length = SPD_IDENTITY_BLOCK
            for controller in CONTROLLER_OFFSETS:
                for address in SPD_HUB_ADDRESSES:
                    if not reader.probe_address(address, controller):
                        continue
                    try:
                        values = reader.read_spd(address, offset, length, controller)
                    except (OSError, TimeoutError, ValueError):
                        continue
                    identity = decode_identity(values)
                    identity.update(address=address, controller=controller)
                    modules.append(identity)
                # DIMMs live on one controller; once it answered, stop probing
                # the other, since probing an empty address blocks.
                if modules:
                    break
    except Exception:
        modules = []

    if use_cache:
        _CACHE.clear()
        _CACHE.append(modules)
    return modules
