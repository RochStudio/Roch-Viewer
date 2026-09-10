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


_reader = None


def read_board_rails(identity=None, reader=None):
    global _reader
    manufacturer, product = board_identity() if identity is None else identity
    if (manufacturer, product) != ("GIGABYTE TECHNOLOGY CO., LTD.", "X870 AORUS TACHYON ICE"):
        from rochviewer.sensors.superio_lpc import read_board_rails as read_nuvoton
        return read_nuvoton()
    try:
        if reader is None:
            if _reader is None:
                from rochviewer.sensors.ite_superio import IteSuperIoReader
                candidate = IteSuperIoReader()
                if not candidate.detect():
                    return {}
                _reader = candidate
            reader = _reader
        # IT8696E VIN6, register 0x26, 12 mV/count. This board returned
        # 118 counts (1.416 V), matching the user's same-boot VDDIO reading.
        # Gigabyte AM5 VIN6 mapping reference: LibreHardwareMonitor,
        # SuperIOHardware.cs (X670 AORUS family). Gate to this tested board;
        # no BIOS voltage sweep was performed to validate other boards.
        value = reader.read_voltage("IT8696E", 0x26)
        if value is None:
            return {}
        return {"vddio_mem": validate_voltage(RAILS_BY_KEY["vddio_mem"], value)}
    except (OSError, RuntimeError, ValueError, TypeError):
        return {}
