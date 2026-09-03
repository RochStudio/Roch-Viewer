"""AMD AM5 System Management Network (SMN) reader.

SMN registers on AMD client platforms are reached through the legacy PCI
configuration mechanism on the root complex device 00:00.0:

    * write the 32-bit SMN address to config register 0x60 (the index)
    * read the 32-bit result from config register 0x64 (the data)

Config space itself is accessed through I/O ports 0xCF8 (address) and
0xCFC (data).  This module wraps that protocol with:

    * vendor gating (device 00:00.0 must report AMD, 0x1022)
    * serialization through a process lock and a named Windows mutex so
      concurrent tools do not clobber the shared CF8/CFC window
    * save/restore of the caller's prior CF8 value
    * a driver-open pre-check via inpoutx64's IsInpOutDriverOpen

There is deliberately **no** public SMN write API.  The only config-space
write performed is the selector write to register 0x60 required to point
the data window at the register being read.

Port I/O is injected (``io``) so the decode/transport logic can be tested
without ever touching hardware.
"""

import threading

# -- PCI config mechanism constants -----------------------------------------
CF8_PORT = 0xCF8              # config address port
CFC_PORT = 0xCFC             # config data port
PCI_CONFIG_ENABLE = 0x80000000

# Root complex 00:00.0 config registers used for SMN access.
VENDOR_REG = 0x00            # DWORD: [15:0] vendor id, [31:16] device id
SMN_INDEX_REG = 0x60        # selector: SMN address goes here
SMN_DATA_REG = 0x64         # data window: SMN value read here

AMD_VENDOR_ID = 0x1022

# RSMU mailbox, addressed over SMN like everything else above.
#
# These live here rather than with the version probe that first used them
# because the viewer needs them too: amd_smu_clocks drives the same mailbox to
# read clocks. Importing them from the probe made the shipped viewer pull that
# whole research module in behind four numbers, which the packaging boundary
# test in test_amd_smu_address_probe exists to prevent.
RSMU_MSG = 0x3B10524
RSMU_RSP = 0x3B10570
RSMU_ARG0 = 0x3B10A40

# TableVersionId. Read-only, and the only command the version probe issues.
RSMU_TABLE_VERSION_COMMAND = 0x05

WAIT_OBJECT_0 = 0x00000000
WAIT_ABANDONED = 0x00000080
WAIT_TIMEOUT = 0x00000102
DEFAULT_MUTEX_NAME = "Global\\Access_PCI"

def _config_address(reg):
    """CF8 value selecting bus 0, device 0, function 0, register ``reg``."""
    return PCI_CONFIG_ENABLE | (reg & 0xFC)


# The mutex and the port I/O live in hardware/lowlevel_io: every transport
# needs them, and two copies would mean two handles onto one DLL and two
# mutexes sharing a name, which is the opposite of what the name is for.
from rochviewer.hardware.lowlevel_io import (
    DEFAULT_MUTEX_NAME,
    InpOutPortIO,
    NamedMutex,
    _config_address,
)


class SmnReader:
    """Serialized, vendor-gated reader for AM5 SMN registers.

    ``io`` must expose ``is_driver_open()``, ``inl(port)`` and
    ``outl(port, value)``.  ``mutex`` is any context manager used to
    serialize with other processes; by default a real named Windows mutex.
    """

    def __init__(self, io=None, mutex=None, mutex_name=DEFAULT_MUTEX_NAME):
        self._io = io if io is not None else InpOutPortIO()
        self._mutex = mutex if mutex is not None else NamedMutex(mutex_name)
        self._lock = threading.Lock()
        self.last_error = ""

    # -- internal config-space helpers -------------------------------------
    def _read_config_dword(self, reg):
        self._io.outl(CF8_PORT, _config_address(reg))
        return self._io.inl(CFC_PORT) & 0xFFFFFFFF

    def _write_config_dword(self, reg, value):
        self._io.outl(CF8_PORT, _config_address(reg))
        self._io.outl(CFC_PORT, value & 0xFFFFFFFF)

    def read_many(self, smn_addresses):
        """Read several SMN dwords in one serialized PCI transaction.

        The named mutex, AMD vendor check, and CF8 save/restore happen once for
        the complete snapshot. Values are ``None`` if the transaction fails;
        genuine zero remains zero.
        """
        addresses = tuple(int(address) & 0xFFFFFFFF for address in smn_addresses)
        results = {address: None for address in addresses}
        self.last_error = ""
        try:
            if not self._io.is_driver_open():
                self.last_error = "InpOut driver is not open"
                return results
            with self._lock, self._mutex:
                prior_cf8 = self._io.inl(CF8_PORT)
                try:
                    vendor = self._read_config_dword(VENDOR_REG) & 0xFFFF
                    if vendor != AMD_VENDOR_ID:
                        self.last_error = (
                            "PCI 00:00.0 vendor is 0x%04X, expected AMD 0x1022"
                            % vendor
                        )
                        return results
                    prior_selector = self._read_config_dword(SMN_INDEX_REG)
                    selector_attempted = False
                    try:
                        for address in addresses:
                            # Mark before the call because a driver can report
                            # failure after partially completing the write.
                            selector_attempted = True
                            self._write_config_dword(SMN_INDEX_REG, address)
                            value = self._read_config_dword(SMN_DATA_REG)
                            if value == 0xFFFFFFFF:
                                raise ValueError(
                                    "SMN 0x%08X returned invalid 0xFFFFFFFF"
                                    % address
                                )
                            results[address] = value
                        return results
                    finally:
                        if selector_attempted:
                            self._write_config_dword(
                                SMN_INDEX_REG, prior_selector
                            )
                finally:
                    self._io.outl(CF8_PORT, prior_cf8)
        except Exception as exc:
            self.last_error = "SMN read failed: %s" % exc
            return {address: None for address in addresses}

    def read(self, smn_address):
        """Return one 32-bit SMN value, or ``None`` on transaction failure."""
        address = int(smn_address) & 0xFFFFFFFF
        return self.read_many((address,))[address]
