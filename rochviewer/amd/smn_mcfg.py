"""AM5 SMN reader through ACPI MCFG / PCIe ECAM.

The public surface is read-only. Each SMN read requires one PCI configuration
write to the AMD SMN index register (0x60), followed by a read from 0x64.
"""

import ctypes
import threading
from ctypes import wintypes

from rochviewer.hardware.pci_mcfg import (
    InpOutPhysicalAccess,
    ecam_address,
    get_mcfg_table,
    parse_mcfg,
    select_allocation,
)
from rochviewer.amd.smn import (
    AMD_VENDOR_ID,
    DEFAULT_MUTEX_NAME,
    InpOutPortIO,
    NamedMutex,
    SMN_DATA_REG,
    SMN_INDEX_REG,
    VENDOR_REG,
)


class InpOutMcfgConfigIO:
    """PCI 00:00.0 config dwords mapped through the ACPI MCFG table."""

    def __init__(self, port_io=None, table=None, segment=0, bus=0):
        self._port_io = port_io or InpOutPortIO()
        self._physical = InpOutPhysicalAccess(self._port_io._dll)
        entries = parse_mcfg(table if table is not None else get_mcfg_table())
        self.allocation = select_allocation(entries, segment, bus)
        self.config_base = ecam_address(
            self.allocation, bus, 0, 0, 0
        )
        raw_write = self._port_io._dll.SetPhysLong
        raw_write.argtypes = [wintypes.LPVOID, wintypes.DWORD]
        raw_write.restype = wintypes.BOOL
        selector_address = self.config_base + SMN_INDEX_REG

        # Close over the one permitted physical destination. No callable in
        # this transport accepts a caller-provided address or config offset.
        def write_selector_value(value):
            if not raw_write(
                ctypes.c_void_p(selector_address),
                wintypes.DWORD(value & 0xFFFFFFFF),
            ):
                raise OSError(
                    "SetPhysLong failed at selector 0x%016X"
                    % selector_address
                )

        self._write_selector_value = write_selector_value

    def is_driver_open(self):
        return self._port_io.is_driver_open()

    def read_dword(self, register):
        if register & 3 or not 0 <= register <= 0xFFC:
            raise ValueError("PCI config dword register is invalid")
        return self._physical.read_dword(self.config_base + register)

    def write_selector(self, value):
        """Write only PCI config offset 0x60, the required SMN selector."""
        self._write_selector_value(value)


class McfgSmnReader:
    """Serialized, AMD-gated, read-only SMN transport over PCIe ECAM."""

    def __init__(
        self,
        config_io=None,
        mutex=None,
        mutex_name=DEFAULT_MUTEX_NAME,
    ):
        self._config = config_io or InpOutMcfgConfigIO()
        self._mutex = mutex if mutex is not None else NamedMutex(mutex_name)
        self._lock = threading.Lock()
        self.last_error = ""

    def read_many(self, smn_addresses):
        addresses = tuple(
            int(address) & 0xFFFFFFFF for address in smn_addresses
        )
        results = {address: None for address in addresses}
        self.last_error = ""
        try:
            if not self._config.is_driver_open():
                self.last_error = "InpOut driver is not open"
                return results
            with self._lock, self._mutex:
                vendor = self._config.read_dword(VENDOR_REG) & 0xFFFF
                if vendor != AMD_VENDOR_ID:
                    self.last_error = (
                        "MCFG PCI 00:00.0 vendor is 0x%04X, "
                        "expected AMD 0x1022" % vendor
                    )
                    return results

                prior_selector = self._config.read_dword(SMN_INDEX_REG)
                selector_attempted = False
                try:
                    for address in addresses:
                        # Mark before the call: a driver can report failure
                        # after partially completing the physical write.
                        selector_attempted = True
                        self._config.write_selector(address)
                        value = self._config.read_dword(
                            SMN_DATA_REG
                        ) & 0xFFFFFFFF
                        if value == 0xFFFFFFFF:
                            raise ValueError(
                                "SMN 0x%08X returned invalid 0xFFFFFFFF"
                                % address
                            )
                        results[address] = value
                    return results
                finally:
                    if selector_attempted:
                        self._config.write_selector(prior_selector)
        except Exception as exc:
            self.last_error = "MCFG SMN read failed: %s" % exc
            return {address: None for address in addresses}

    def read(self, smn_address):
        address = int(smn_address) & 0xFFFFFFFF
        return self.read_many((address,))[address]
