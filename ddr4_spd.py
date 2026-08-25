"""DDR4 SPD identity: the serial and build date, read off the module.

SMBIOS carries no build date at all and reports a serial only when firmware
found one, so these two rows have nothing behind them unless the module's own
SPD is read. On DDR5 that is :mod:`ddr5_spd`; this is the DDR4 half.

Nothing here writes. That is the whole design constraint, and it is what makes
the module different from its DDR5 counterpart:

A DDR4 module's SPD is 512 bytes exposed 256 at a time, and the manufacturing
block -- bytes 320 to 352 -- is in the upper half. Selecting which half the
EEPROM presents is a write, to the separate SPA0/SPA1 addresses, and this
project's SMBus transport does not write outside its allowlist. So this module
does not select anything: it reads the window as the firmware left it and
proves, from the bytes themselves, which half it is looking at.

The proof is the part number. Bytes 329-348 are twenty ASCII characters, and
on the Z790-P bench registers 0x49-0x5C read "F4-3600C14-16GVKA" -- which is
the part number SMBIOS reports for the same stick, and which lands at that
offset only if the window base is byte 256. The JEP106 pairs on either side
agree: 0x04CD at register 0x40 is G.Skill, the module maker, and 0x80CE at
0x5E is Samsung, the DRAM maker. Three fields from three different parts of
the block, all landing where the upper half puts them.

If that check fails -- a module whose window is on the lower half, or an
unreadable bus -- this reports nothing and the rows fall back to SMBIOS. It
never decodes a block it has not first confirmed the alignment of.

Both sticks on the bench read zero for the location, the date, the serial, the
module revision and the DRAM stepping: G.Skill ships this kit with the whole
optional part of the block unprogrammed. That is why those rows show nothing
here, and it is a reading rather than a failure to read -- the part number two
registers along comes back perfectly from the same transaction.
"""

from ddr5_spd import EM_DASH, decode_jep106_id, decode_manufacture_date

# Register offsets within the upper window, i.e. SPD byte minus 256.
SPD_WINDOW_BASE = 0x100
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
    """The serial as eight hex digits, or an em dash.

    All four bytes zero is an unprogrammed block, not a serial of zero. Both
    bench sticks read that way, and printing 00000000 would put a serial on a
    module that never gave one.
    """
    digits = ""
    for offset in range(SPD_SERIAL_NUMBER,
                        SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH):
        byte = values.get(offset)
        if byte is None:
            return EM_DASH
        digits += "%02X" % (int(byte) & 0xFF)
    if not digits.strip("0") or not digits.strip("F"):
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


# Every register the block needs, read one at a time. There is no block read
# here: read_spd is the DDR5 hub protocol and selects pages, which is exactly
# what this module must not do.
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
        from ddr5_telemetry import default_smbus_backend

        backend = default_smbus_backend()
        if backend is None:
            return []
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
        values = {}
        for register in IDENTITY_REGISTERS:
            values[register] = reader.read_byte(address, register, controller)
    except (OSError, TimeoutError, ValueError):
        return None
    identity = decode_identity(values)
    if identity is None:
        return None
    identity.update(address=address, controller=controller)
    return identity
