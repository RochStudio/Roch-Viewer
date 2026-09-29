"""Native AM5 rail routing. Never substitute a DIMM PMIC voltage for VDDIO."""
from functools import lru_cache

from rochviewer.sensors.voltage_rails import RAILS_BY_KEY, validate_voltage


@lru_cache(maxsize=1)
def board_identity():
    try:
        import wmi
        board = wmi.WMI().Win32_BaseBoard()[0]
        return str(board.Manufacturer).strip().upper(), str(board.Product).strip().upper()
    except Exception:
        return "", ""


X870_TACHYON = ("GIGABYTE TECHNOLOGY CO., LTD.", "X870 AORUS TACHYON ICE")
CHIP = "IT8696E"

_reader = None


def _board_reader(reader=None):
    """The IT8696E reader for this board, detected once, or None."""
    global _reader
    if reader is not None:
        return reader
    if _reader is None:
        from rochviewer.sensors.ite_superio import IteSuperIoReader
        candidate = IteSuperIoReader()
        if not candidate.detect():
            return None
        _reader = candidate
    return _reader


def read_board_rails(identity=None, reader=None):
    manufacturer, product = board_identity() if identity is None else identity
    if (manufacturer, product) != X870_TACHYON:
        from rochviewer.sensors.superio_lpc import read_board_rails as read_nuvoton
        return read_nuvoton()
    try:
        reader = _board_reader(reader)
        if reader is None:
            return {}
        # IT8696E VIN6, register 0x26, 12 mV/count. This board returned
        # 118 counts (1.416 V), matching the user's same-boot VDDIO reading.
        # Gigabyte AM5 VIN6 mapping reference: LibreHardwareMonitor,
        # SuperIOHardware.cs (X670 AORUS family). Gate to this tested board;
        # no BIOS voltage sweep was performed to validate other boards.
        value = reader.read_voltage(CHIP, 0x26)
        if value is None:
            return {}
        return {"vddio_mem": validate_voltage(RAILS_BY_KEY["vddio_mem"], value)}
    except (OSError, RuntimeError, ValueError, TypeError):
        return {}


# --- The rest of the IT8696E on this board: supplies, fans, one more probe.
#
# Every channel below was read on the X870 AORUS TACHYON ICE and decoded to
# what a reference tool showed for the same chip on the same boot. The dividers are
# ITE's usual ones for these inputs (12 mV per count, then the resistor
# ratio): +12V 168 x 12 mV x 6 = 12.096 V against the reference's 12.096, +3.3V
# 167 x x1.65 = 3.307 against 3.307, 3VSB, VBAT and AVCC3 x2 = 3.336, 3.048
# and 3.072 exactly, +5V 161 x x2.5 = 4.830 against 4.860 a moment apart.
#
# 0x24 (1.272 V) and 0x25 (1.116 V) are left out. The reference names them
# "iGPU VAXG" and "CPU VCCIN_AUX", Intel rails an AM5 board does not have,
# and naming them properly needs a BIOS change to see which one moves.
#
# (key, label, register, divider, plausible band in volts)
BOARD_VOLTAGES = (
    ("board_vcore", "Vcore (board)", 0x20, 1.0, (0.3, 1.8)),
    ("plus12v", "+12V", 0x22, 6.0, (10.8, 13.2)),
    ("plus5v", "+5V", 0x23, 2.5, (4.5, 5.5)),
    ("plus3v3", "+3.3V", 0x21, 1.65, (3.0, 3.6)),
    ("v3vsb", "3VSB", 0x27, 2.0, (3.0, 3.6)),
    ("vbat", "VBAT", 0x28, 2.0, (2.0, 3.6)),
    ("avcc3", "AVCC3", 0x2F, 2.0, (3.0, 3.6)),
)

# Fan tachometers: a 16-bit count split over a low and a high register, fans
# 4 to 6 in ITE's extended block. The headers are named in the order
# the reference lists them. CPU Fan and CPU_OPT are the two with fans fitted on
# the bench, and their counts decode to 1403 and 3648 RPM against its 1510 and
# 3609; the other four read 0xFFFF, a header with nothing on it, which is why
# which System Fan is which has not been seen here.
#
# (key, label, low register, high register)
BOARD_FANS = (
    ("cpu_fan", "CPU Fan", 0x0D, 0x18),
    ("sys_fan1", "System Fan 1", 0x0E, 0x19),
    ("sys_fan2", "System Fan 2", 0x0F, 0x1A),
    ("sys_fan3", "System Fan 3", 0x80, 0x81),
    ("cpu_opt", "CPU_OPT", 0x82, 0x83),
    ("fan6", "Fan 6", 0x84, 0x85),
)

# Two thermistors: the chipset, 37 C beside the reference's "PCH" 37 C, and the
# PCIe x16 slot, 34 C beside its 34 C.
#
# (key, register)
BOARD_TEMPERATURES = (("pch", 0x2A), ("pciex16", 0x2C))

# ITE's tachometer clock over two pulses a revolution.
TACH_CLOCK = 1_350_000


def fan_rpm(low, high):
    """RPM from a tachometer count, or None for a header with no fan."""
    if low is None or high is None:
        return None
    count = ((int(high) & 0xFF) << 8) | (int(low) & 0xFF)
    if count in (0, 0xFFFF):
        return None
    return round(TACH_CLOCK / (2 * count))


def _board_temperature(raw):
    """A thermistor reading in Celsius, or None for an unconnected probe."""
    from rochviewer.sensors.ite_superio import (
        TEMPERATURE_MAX_C, TEMPERATURE_MIN_C,
    )

    if raw is None or not TEMPERATURE_MIN_C <= raw <= TEMPERATURE_MAX_C:
        return None
    return float(raw)


def read_board_monitor(identity=None, reader=None):
    """This board's supplies, fans and slot probe, as one dict. Read-only.

    Keys are those of BOARD_VOLTAGES, BOARD_FANS and BOARD_TEMPERATURES. A
    reading outside its band, and a fan header with nothing on it, are left
    out rather than shown as a number. Any other board gets an empty dict:
    the map is this board's, confirmed on it, and does not transfer.
    """
    manufacturer, product = board_identity() if identity is None else identity
    if (manufacturer, product) != X870_TACHYON:
        return {}
    try:
        reader = _board_reader(reader)
        if reader is None:
            return {}
        # Every register this reads, in one hold of the bus.
        registers = [register for _k, _l, register, _d, _b in BOARD_VOLTAGES]
        for _key, _label, low_register, high_register in BOARD_FANS:
            registers += [low_register, high_register]
        registers += [register for _key, register in BOARD_TEMPERATURES]
        raw = reader.read_registers(CHIP, registers)
        step = reader.voltage_step(CHIP)
        if not raw or not step:
            return {}
        found = {}
        for key, _label, register, divider, (low, high) in BOARD_VOLTAGES:
            if raw.get(register) is None:
                continue
            volts = raw[register] * step * divider
            if low <= volts <= high:
                found[key] = volts
        for key, _label, low_register, high_register in BOARD_FANS:
            rpm = fan_rpm(raw.get(low_register), raw.get(high_register))
            if rpm is not None:
                found[key] = rpm
        for key, register in BOARD_TEMPERATURES:
            celsius = _board_temperature(raw.get(register))
            if celsius is not None:
                found[key] = celsius
        return found
    except (OSError, RuntimeError, ValueError, TypeError):
        return {}
