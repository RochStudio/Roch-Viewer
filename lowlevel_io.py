"""Serialised low-level access: a named mutex and inpoutx64 port I/O.

Two pieces the readers share:

    * :class:`NamedMutex`, the Windows named mutex the mainstream monitoring
      tools take before touching the shared CF8/CFC configuration window, so
      two tools do not clobber each other's selector mid-read.
    * :class:`InpOutPortIO`, the port-I/O wrapper over inpoutx64, including
      the driver-open pre-check.

Both were factored out of the AMD SMN reader, which is where they were first
needed. Nothing here is vendor-specific, and there is deliberately no write
API beyond the port primitives the transports need for their own selectors.
"""

import ctypes
import os
import threading
from ctypes import wintypes

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
WAIT_FAILED = 0xFFFFFFFF

DEFAULT_MUTEX_NAME = "Global\\Access_PCI"

from driver_path import find_driver

_DLL_PATH = find_driver()


def _config_address(reg):
    """CF8 value selecting bus 0, device 0, function 0, register ``reg``."""
    return PCI_CONFIG_ENABLE | (reg & 0xFC)


class NamedMutex:
    """Context manager around a named Windows mutex (kernel32)."""

    def __init__(self, name=DEFAULT_MUTEX_NAME, timeout_ms=5000):
        self._name = name
        self._timeout_ms = timeout_ms
        self._handle = None
        self._owned = False
        self._k32 = ctypes.WinDLL("kernel32", use_last_error=True)
        self._k32.CreateMutexW.argtypes = [wintypes.LPVOID, wintypes.BOOL,
                                           wintypes.LPCWSTR]
        self._k32.CreateMutexW.restype = wintypes.HANDLE
        self._k32.WaitForSingleObject.argtypes = [wintypes.HANDLE,
                                                  wintypes.DWORD]
        self._k32.WaitForSingleObject.restype = wintypes.DWORD
        self._k32.ReleaseMutex.argtypes = [wintypes.HANDLE]
        self._k32.ReleaseMutex.restype = wintypes.BOOL
        self._k32.CloseHandle.argtypes = [wintypes.HANDLE]
        self._k32.CloseHandle.restype = wintypes.BOOL

    def __enter__(self):
        self._handle = self._k32.CreateMutexW(None, False, self._name)
        if not self._handle:
            raise OSError(ctypes.get_last_error(), "CreateMutexW failed")
        wait_result = self._k32.WaitForSingleObject(
            self._handle, self._timeout_ms
        )
        if wait_result in (WAIT_OBJECT_0, WAIT_ABANDONED):
            self._owned = True
            return self
        self._k32.CloseHandle(self._handle)
        self._handle = None
        if wait_result == WAIT_TIMEOUT:
            raise TimeoutError("Timed out waiting for PCI configuration mutex")
        raise OSError(ctypes.get_last_error(), "WaitForSingleObject failed")

    def __exit__(self, *exc):
        if self._handle:
            if self._owned:
                self._k32.ReleaseMutex(self._handle)
            self._k32.CloseHandle(self._handle)
            self._handle = None
            self._owned = False
        return False


class InpOutPortIO:
    """Real port I/O backed by the bundled inpoutx64.dll."""

    def __init__(self, dll_path=None):
        # Resolved on use rather than at import: the module is imported
        # before anyone has had a chance to put the driver anywhere, and a
        # path frozen at import time cannot notice it arriving.
        dll_path = dll_path or find_driver()
        if not dll_path or not os.path.exists(dll_path):
            from driver_path import missing_message

            raise FileNotFoundError(missing_message())
        self._dll = ctypes.WinDLL(dll_path)
        self._dll.IsInpOutDriverOpen.argtypes = []
        self._dll.IsInpOutDriverOpen.restype = wintypes.UINT
        self._dll.DlPortReadPortUlong.argtypes = [wintypes.ULONG]
        self._dll.DlPortReadPortUlong.restype = wintypes.ULONG
        self._dll.DlPortWritePortUlong.argtypes = [wintypes.ULONG,
                                                   wintypes.ULONG]
        self._dll.DlPortWritePortUlong.restype = None
        # Byte-width access, used by the SMBus and Super I/O transports.
        self._dll.DlPortReadPortUchar.argtypes = [wintypes.USHORT]
        self._dll.DlPortReadPortUchar.restype = ctypes.c_ubyte
        self._dll.DlPortWritePortUchar.argtypes = [wintypes.USHORT,
                                                   ctypes.c_ubyte]
        self._dll.DlPortWritePortUchar.restype = None

    def is_driver_open(self):
        return bool(self._dll.IsInpOutDriverOpen())

    def inl(self, port):
        return int(self._dll.DlPortReadPortUlong(port)) & 0xFFFFFFFF

    def outl(self, port, value):
        self._dll.DlPortWritePortUlong(port, value & 0xFFFFFFFF)

    def inb(self, port):
        return int(self._dll.DlPortReadPortUchar(int(port) & 0xFFFF)) & 0xFF

    def outb(self, port, value):
        self._dll.DlPortWritePortUchar(int(port) & 0xFFFF, int(value) & 0xFF)


# The byte-oriented transports historically had their own wrapper; it is the
# same DLL handle and the same driver-open contract, so it is just this class.
InpOutByteIO = InpOutPortIO


