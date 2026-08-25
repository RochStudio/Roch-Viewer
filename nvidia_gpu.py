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

"""What the graphics card reports about itself.

CPU-Z's Graphics tab is part reading and part database. This module ships the
reading half, from three sources that need no driver of our own: the adapter's
PNP device id for what the card is, and NVAPI and NVML for what it holds.

The NVAPI entry points are not in NVIDIA's published header -- they are
resolved by ID through nvapi_QueryInterface, the same way GPU-Z and HWiNFO
reach them -- so every value was checked against CPU-Z's report for this card
before being trusted:

    board GIGABYTE (subsystem 0x1458), revision A1, AD104-A, 5888 cores,
    12282 MB of GDDR6X by Micron on a 192-bit bus.

All of them agreed, and the two a second interface also answers (cores, via
NVML; bus width, via GetRamBusWidth) agreed with themselves as well.

Two values on that tab cannot be read at all: ROP and TM unit counts, which
have no entry point on this driver, and a chip's SKU, which has none anywhere.
Both come from GPU_DEVICE_TABLE, and are the only claims here rather than
readings.
"""

import ctypes
import re
import threading

NVIDIA_VENDOR_ID = 0x10DE

# PCI subsystem vendors, i.e. who assembled the board. 0x1458 is measured on
# this bench, where CPU-Z names it GIGABYTE Technology; the rest are the other
# major add-in-board vendors. Unlisted vendors print their ID rather than a
# guess, as elsewhere in this project.
BOARD_VENDORS = {
    0x1043: "ASUSTeK Computer",
    0x1458: "GIGABYTE Technology",
    0x1462: "MSI",
    0x1569: "Palit Microsystems",
    0x196E: "PNY",
    0x19DA: "Zotac",
    0x1B4C: "KFA2",
    0x3842: "EVGA",
    0x7377: "Colorful",
    0x10DE: "NVIDIA",
}

# Per PCI device ID: the marketed chip name, and the ROP and TM unit counts.
# Nothing on the card reports any of the three, so these are claims rather
# than readings. An unlisted device gets no row: guessing from a neighbouring
# part is how a table like this starts being wrong quietly.
#
# The driver does report a chip name -- "AD104-A", the core family -- and it
# is used when this table has no entry. CPU-Z shows the SKU, "AD104-250".
#
# 184 TM units, not 368: CPU-Z's window says 184 while its own text report
# says 368 for the same card. The window agrees with every other source.
# TM units follow the shader count on every Ada part: four per SM, and an SM
# is 128 shaders. The 4070's listed 5888 / 128 = 46 SMs gives 184, which is
# the number CPU-Z's window shows, so the rule reproduces the entry that was
# checked against hardware. ROP counts do not follow from anything readable --
# they belong to the raster partitions the SKU was cut with -- so each is the
# vendor's figure for that part, cross-checked against GPU-Z's database.
#
# 0x2782 is measured on this bench: the card reports 7680 shaders, which is 60
# SMs and so 240 TM units, and GPU-Z reads 80 ROPs against the same card.
GPU_DEVICE_TABLE = {
    0x2684: ("AD102-300", 176, 512),       # GeForce RTX 4090
    0x2704: ("AD103-300", 112, 304),       # GeForce RTX 4080
    0x2782: ("AD104-400", 80, 240),        # GeForce RTX 4070 Ti
    0x2786: ("AD104-250", 64, 184),        # GeForce RTX 4070
}

# NVML architecture -> process node, keyed on the architecture because that is
# what the node is a property of. 8 is Ada, measured here against CPU-Z's 4 nm;
# the others are the neighbouring client architectures. A datacentre part of
# the same generation can be built on a different node, so an unlisted
# architecture reports nothing.
NVML_ARCHITECTURE_NODES = {
    6: "12 nm",               # Turing
    7: "8 nm",                # Ampere, GA10x client
    8: "4 nm",                # Ada -- verified against CPU-Z on this bench
    10: "4 nm",               # Blackwell, GB20x client
}

# NVAPI's memory type and maker enumerations, neither of which is published.
# Only the values this bench returned are named: 15 alongside CPU-Z reporting
# GDDR6X, and 10 alongside Micron. That measurement contradicted the ordering
# these enumerations are usually quoted with, which is why nothing here is
# filled in from memory -- an unmeasured code prints as itself.
NVAPI_RAM_TYPES = {15: "GDDR6X"}
NVAPI_RAM_MAKERS = {10: "Micron"}

NVAPI_QUERIES = {
    "initialize": 0x0150E828,
    "enum_gpus": 0xE5AC921F,
    "core_count": 0xC7026A87,
    "short_name": 0xD988F0F3,
    "ram_type": 0x57F7CAAC,
    "ram_maker": 0x42AEA16A,
    "ram_bus_width": 0x7975C581,
    "frame_buffer_kb": 0x46FBEB03,
    # Core voltage. Undocumented, like the four above it, and confirmed the
    # same way: it returns 960000 uV where HWiNFO reads 0.960 V on this card.
    "core_voltage": 0x465F9BCF,
}

# Built once rather than per call: a CFUNCTYPE is a type, and rebuilding it on
# every read costs more than the call it wraps.
_PROTO_STATUS = ctypes.CFUNCTYPE(ctypes.c_int)
_PROTO_ENUM = ctypes.CFUNCTYPE(
    ctypes.c_int, ctypes.POINTER(ctypes.c_void_p), ctypes.POINTER(ctypes.c_uint32)
)
_PROTO_UINT_OUT = ctypes.CFUNCTYPE(
    ctypes.c_int, ctypes.c_void_p, ctypes.POINTER(ctypes.c_uint32)
)
_PROTO_TEXT_OUT = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_char_p)
_PROTO_STRUCT_OUT = ctypes.CFUNCTYPE(
    ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p
)


class _NvapiVoltStatus(ctypes.Structure):
    """The core-voltage status block: the value, with padding either side."""

    _fields_ = [("version", ctypes.c_uint32),
                ("flags", ctypes.c_uint32),
                ("unknown1", ctypes.c_uint32 * 8),
                ("value_uv", ctypes.c_uint32),
                ("unknown2", ctypes.c_uint32 * 8)]

# "PCI\VEN_10DE&DEV_2786&SUBSYS_40C61458&REV_A1\4&256A0AA8&0&0008"
_PNP_IDENTITY = re.compile(
    r"VEN_([0-9A-F]{4})&DEV_([0-9A-F]{4})&SUBSYS_([0-9A-F]{4})([0-9A-F]{4})"
    r"&REV_([0-9A-F]{2})",
    re.IGNORECASE,
)

_CACHE = []


def _own_pnp_device_ids():
    """Ask WMI for the display adapters, when the caller has not already."""
    import wmi

    found = []
    for adapter in wmi.WMI().Win32_VideoController():
        name = (getattr(adapter, "Name", "") or "").strip()
        if name and "Basic Display" not in name:
            found.append(str(getattr(adapter, "PNPDeviceID", "") or ""))
    return found


def _adapter_identity(pnp_device_ids=None):
    """Return the display adapter's PCI identity, or None.

    Read from the PNP device id Windows already holds, which carries all four
    fields: vendor, device, subsystem and revision. Walking PCI configuration
    space for the same four cost a map/unmap pair per dword across 64
    functions, and returned the first VGA-class device it found -- the
    integrated graphics, on a machine that has both.

    The ids are taken from the caller where it has them. Opening a WMI
    connection of our own costs about 90 ms, and the one caller in this
    project has already queried and cached that class for the card's name.
    """
    try:
        if pnp_device_ids is None:
            pnp_device_ids = _own_pnp_device_ids()
        for pnp_device_id in pnp_device_ids:
            found = _PNP_IDENTITY.search(str(pnp_device_id or ""))
            if not found:
                continue
            device, subsystem_device, subsystem_vendor = (
                found.group(2), found.group(3), found.group(4)
            )
            return {
                "vendor_id": int(found.group(1), 16),
                "device_id": int(device, 16),
                # SUBSYS is device then vendor, high half first.
                "subsystem_vendor_id": int(subsystem_vendor, 16),
                "subsystem_device_id": int(subsystem_device, 16),
                "revision": int(found.group(5), 16),
            }
    except Exception:
        pass
    return None


class _Nvapi:
    """The handful of NVAPI entry points this module uses."""

    def __init__(self):
        self._resolved = {}
        library = ctypes.WinDLL("nvapi64.dll")
        query = library.nvapi_QueryInterface
        query.argtypes = [ctypes.c_uint32]
        query.restype = ctypes.c_void_p
        for name, identifier in NVAPI_QUERIES.items():
            pointer = query(identifier)
            if pointer:
                self._resolved[name] = pointer
        if "initialize" not in self._resolved or "enum_gpus" not in self._resolved:
            raise OSError("NVAPI is present but its entry points are not")
        if _PROTO_STATUS(self._resolved["initialize"])() != 0:
            raise OSError("NvAPI_Initialize failed")
        handles = (ctypes.c_void_p * 64)()
        count = ctypes.c_uint32(0)
        status = _PROTO_ENUM(self._resolved["enum_gpus"])(
            handles, ctypes.byref(count)
        )
        if status != 0 or not count.value:
            raise OSError("no NVIDIA GPU enumerated")
        self.gpu = handles[0]

    def unsigned(self, name):
        """Call an entry point shaped (handle, uint32*), or return None."""
        pointer = self._resolved.get(name)
        if not pointer:
            return None
        value = ctypes.c_uint32(0)
        status = _PROTO_UINT_OUT(pointer)(self.gpu, ctypes.byref(value))
        # A negative status is NVAPI reporting the call unsupported on this
        # part; the out parameter is meaningless then and must not be used.
        return None if status != 0 else value.value

    def core_voltage_uv(self):
        """The core rail in microvolts, or None.

        Shaped (handle, struct*) rather than the uint-out shape above, so it
        does not go through unsigned(). The struct is the layout the open
        implementations use: version, flags, eight words this does not read,
        the value, and eight more. Version is size with 1 in the high half,
        which is how every NVAPI struct carries its version.
        """
        pointer = self._resolved.get("core_voltage")
        if not pointer:
            return None
        status = _NvapiVoltStatus()
        status.version = ctypes.sizeof(_NvapiVoltStatus) | (1 << 16)
        if _PROTO_STRUCT_OUT(pointer)(self.gpu, ctypes.byref(status)) != 0:
            return None
        return status.value_uv or None

    def text(self, name):
        """Call an entry point shaped (handle, char*), or return None."""
        pointer = self._resolved.get(name)
        if not pointer:
            return None
        buffer = ctypes.create_string_buffer(64)
        if _PROTO_TEXT_OUT(pointer)(self.gpu, buffer) != 0:
            return None
        return buffer.value.decode("ascii", "ignore").strip() or None


class _NvmlMemory(ctypes.Structure):
    """nvmlMemory_t and nvmlBAR1Memory_t: both are three 64-bit totals."""

    _fields_ = [("total", ctypes.c_ulonglong),
                ("free", ctypes.c_ulonglong),
                ("used", ctypes.c_ulonglong)]


def _load_nvml():
    """The NVML library, or None.

    On PATH for a normal driver install, and under NVSMI for the older
    layout that did not put it there.
    """
    for name in ("nvml.dll",
                 "C:\\Program Files\\NVIDIA Corporation"
                 "\\NVSMI\\nvml.dll"):
        try:
            return ctypes.WinDLL(name)
        except OSError:
            continue
    return None


def _nvml_query():
    """Return what NVML reports about the first GPU, as a dict.

    Opened once for everything wanted from it: each init/shutdown pair costs
    more than all the calls between them.
    """
    nvml = _load_nvml()
    if nvml is None or nvml.nvmlInit_v2() != 0:
        return {}
    found = {}
    try:
        version = ctypes.create_string_buffer(80)
        if nvml.nvmlSystemGetDriverVersion(version, 80) == 0:
            text = version.value.decode("ascii", "ignore").strip()
            if text:
                found["driver_version"] = text

        handle = ctypes.c_void_p()
        if nvml.nvmlDeviceGetHandleByIndex_v2(0, ctypes.byref(handle)) != 0:
            return found

        architecture = ctypes.c_uint32(0)
        if nvml.nvmlDeviceGetArchitecture(
            handle, ctypes.byref(architecture)
        ) == 0:
            found["architecture"] = architecture.value

        # Resizable BAR, without touching a BAR register. Sizing one the usual
        # way means writing all-ones to it, which is not something to do to a
        # live display; the aperture NVML reports says the same thing. Off, it
        # is the legacy 256 MB window whatever the card holds. On, it spans
        # the frame buffer.
        bar1, memory = _NvmlMemory(), _NvmlMemory()
        if (
            nvml.nvmlDeviceGetBAR1MemoryInfo(handle, ctypes.byref(bar1)) == 0
            and nvml.nvmlDeviceGetMemoryInfo(handle, ctypes.byref(memory)) == 0
            and bar1.total and memory.total
        ):
            found["bar1_total"] = bar1.total
            found["frame_buffer_total"] = memory.total
        return found
    except Exception:
        # An NVML too old for one of these exports raises rather than
        # returning a status. Whatever was gathered before that still stands.
        return found
    finally:
        try:
            nvml.nvmlShutdown()
        except Exception:
            pass


def _named(table, code, prefix):
    if code is None:
        return None
    return table.get(int(code), "%s %d" % (prefix, int(code)))


def read_gpu(refresh=False, pnp_device_ids=None):
    """Return what the card reports, as a dict of display strings.

    Cached: none of it changes while the machine runs, and the callers are row
    getters that run at startup and again on every Advanced window tick.
    Missing keys mean the card did not answer, and callers show nothing rather
    than a placeholder.
    """
    if _CACHE and not refresh:
        return _CACHE[0]

    found = {}
    try:
        found = _read_gpu(pnp_device_ids)
    except Exception:
        found = {}
    # Written on every exit, including the failed ones. Cached only on success,
    # a machine whose driver lacks one export would repeat the whole probe --
    # two DLL loads and an NVAPI init -- for all thirteen rows, every second.
    _CACHE[:] = [found]
    return found


def _read_gpu(pnp_device_ids=None):
    found = {}
    pci = _adapter_identity(pnp_device_ids)
    if pci and pci["vendor_id"] != NVIDIA_VENDOR_ID:
        # An AMD or Intel card: the board vendors below are add-in-board
        # makers who ship both brands, and NVAPI will not answer for it, so
        # reporting half a row would be reporting someone else's card.
        return found
    if pci:
        found["revision"] = "%02X" % pci["revision"]
        vendor = pci["subsystem_vendor_id"]
        found["board_manufacturer"] = BOARD_VENDORS.get(
            vendor, "0x%04X" % vendor
        )
        listed = GPU_DEVICE_TABLE.get(pci["device_id"])
        if listed:
            code_name, rops, tmus = listed
            found["code_name"] = code_name
            found["rops_tmus"] = "%d / %d" % (rops, tmus)

    nvml = _nvml_query()
    if nvml.get("driver_version"):
        found["driver_version"] = nvml["driver_version"]
    node = NVML_ARCHITECTURE_NODES.get(nvml.get("architecture"))
    if node:
        found["technology"] = node
    bar1, frame_buffer = nvml.get("bar1_total"), nvml.get("frame_buffer_total")
    if bar1 and frame_buffer:
        # Enabled means the aperture reaches the frame buffer. Compared with
        # room to spare rather than exactly: the aperture is a power of two
        # and the frame buffer is not, so an enabled card can report slightly
        # either side of its own memory size.
        found["resizable_bar"] = (
            "Enabled" if bar1 >= frame_buffer * 0.9 else "Disabled"
        )

    try:
        nvapi = _Nvapi()
    except Exception:
        return found

    # The core family, kept only where the table above has no SKU for this
    # device -- a reading is better than nothing, but the SKU is finer.
    code_name = nvapi.text("short_name")
    if code_name and "code_name" not in found:
        found["code_name"] = code_name
    cores = nvapi.unsigned("core_count")
    if cores:
        found["cores"] = str(cores)
    # Reported in kilobytes. Shown in gigabytes, the same figure CPU-Z prints:
    # this card's 12576256 KB is 11.99 GB, not the 12 GB it is sold as,
    # because part of the frame buffer is not reported here.
    frame_buffer_kb = nvapi.unsigned("frame_buffer_kb")
    if frame_buffer_kb:
        found["memory_size"] = "%.2f GB" % (frame_buffer_kb / 1048576.0)
    width = nvapi.unsigned("ram_bus_width")
    if width:
        found["bus_width"] = "%d bits" % width
    for key, table, query, prefix in (
        ("memory_type", NVAPI_RAM_TYPES, "ram_type", "Type"),
        ("memory_vendor", NVAPI_RAM_MAKERS, "ram_maker", "Maker"),
    ):
        named = _named(table, nvapi.unsigned(query), prefix)
        if named:
            found[key] = named
    return found


# --- Live card readings, for the Telemetry window.
#
# Separate from read_gpu above, which answers what the card *is* and is cached
# for the run. These move, so they are read on every tick.
#
# NVML is held open rather than initialised per call: an init/shutdown pair
# costs more than every reading between them, and the window ticks once a
# second. The AM5 side reads the same figures through AMD's own libraries; the
# labels match it wherever the two report the same thing, so a row means one
# thing across both platforms.
NVML_TEMPERATURE_GPU = 0
NVML_CLOCK_GRAPHICS = 0
NVML_CLOCK_MEM = 2
NVML_CLOCK_VIDEO = 3

# One live session: [] before the first attempt, [None] once it has failed.
_SENSOR_SESSION = []
_SENSOR_LOCK = threading.Lock()


class _NvmlUtilization(ctypes.Structure):
    _fields_ = [("gpu", ctypes.c_uint32), ("memory", ctypes.c_uint32)]


def _sensor_session():
    """``(nvml, handle)`` for the first card, or None. Opened once."""
    if _SENSOR_SESSION:
        return _SENSOR_SESSION[0]
    session = None
    try:
        nvml = _load_nvml()
        if nvml is not None and nvml.nvmlInit_v2() == 0:
            handle = ctypes.c_void_p()
            if nvml.nvmlDeviceGetHandleByIndex_v2(0, ctypes.byref(handle)) == 0:
                session = (nvml, handle)
            else:
                nvml.nvmlShutdown()
    except Exception:
        session = None
    _SENSOR_SESSION.append(session)
    return session


def read_gpu_sensors():
    """Live readings keyed by row name, or {} when the card cannot be read.

    A reading NVML declines is left out rather than zeroed: a card that does
    not report a fan is not a card whose fan is stopped.
    """
    with _SENSOR_LOCK:
        session = _sensor_session()
        if session is None:
            return {}
        nvml, handle = session
        found = {}

        def unsigned(call, *args):
            value = ctypes.c_uint32(0)
            try:
                if call(handle, *args, ctypes.byref(value)) == 0:
                    return value.value
            except Exception:
                pass
            return None

        try:
            temperature = unsigned(nvml.nvmlDeviceGetTemperature,
                                   NVML_TEMPERATURE_GPU)
            if temperature is not None:
                found["GPU Temperature"] = "%d °C" % temperature

            for label, clock in (("GPU Clock", NVML_CLOCK_GRAPHICS),
                                 ("GPU Memory Clock", NVML_CLOCK_MEM),
                                 ("GPU Video Clock", NVML_CLOCK_VIDEO)):
                megahertz = unsigned(nvml.nvmlDeviceGetClockInfo, clock)
                if megahertz is not None:
                    found[label] = "%d MHz" % megahertz

            # Milliwatts from NVML both times. Board power is what the card
            # is drawing; the limit is what it is allowed, and the pair is
            # only useful together -- which is why AM5 carries the limit too.
            milliwatts = unsigned(nvml.nvmlDeviceGetPowerUsage)
            if milliwatts is not None:
                found["GPU Board Power (TBP)"] = "%.1f W" % (milliwatts / 1000.0)
            limit = unsigned(nvml.nvmlDeviceGetEnforcedPowerLimit)
            if limit is not None:
                found["GPU Board Power Limit"] = "%.0f W" % (limit / 1000.0)

            # NVML reports the fan as a percentage of its range. AM5 has both
            # RPM and PWM from ADL; this card exposes only the percentage, so
            # it takes the PWM name rather than claiming an RPM it never read.
            fan = unsigned(nvml.nvmlDeviceGetFanSpeed)
            if fan is not None:
                found["GPU Fan PWM"] = "%d %%" % fan

            utilization = _NvmlUtilization()
            if nvml.nvmlDeviceGetUtilizationRates(
                    handle, ctypes.byref(utilization)) == 0:
                found["GPU Utilization"] = "%d %%" % utilization.gpu

            memory = _NvmlMemory()
            if nvml.nvmlDeviceGetMemoryInfo(handle, ctypes.byref(memory)) == 0:
                found["GPU Memory Used"] = "%d MB" % (memory.used // 1048576)
        except Exception:
            return found

        # Core voltage is NVAPI's, not NVML's. Its own session, opened once
        # and kept: a card whose driver has NVML but not NVAPI still gets
        # everything above.
        microvolts = _voltage_session_read()
        if microvolts:
            found["GPU Core Voltage"] = "%.3f V" % (microvolts / 1e6)
        return found


# The NVAPI session the voltage row uses: [] before the first attempt, [None]
# once it has failed, so a card without it is asked once rather than every
# tick.
_VOLTAGE_SESSION = []


def _voltage_session_read():
    if not _VOLTAGE_SESSION:
        try:
            _VOLTAGE_SESSION.append(_Nvapi())
        except Exception:
            _VOLTAGE_SESSION.append(None)
    session = _VOLTAGE_SESSION[0]
    if session is None:
        return None
    try:
        return session.core_voltage_uv()
    except Exception:
        return None
