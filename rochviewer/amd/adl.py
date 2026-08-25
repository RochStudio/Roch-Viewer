"""What AMD's own driver library will tell us about the card.

The mirror of what the Intel profile does with NVAPI: the vendor ships a
user-mode library alongside its driver, and it answers questions no register
this project can reach will. atiadlxx.dll is that library for Radeon.

It is the only source found for the card's memory vendor. Everything else was
tried first and closed: the expansion ROM BAR is unassigned, so there is no
VBIOS image to parse; MC_SEQ_MISC0, where AMD tools used to read the vendor
nibble, answers all-ones on RDNA4 -- and that is a real answer, since other
offsets in the same aperture return live values; and the driver's registry
subtree carries no vendor string anywhere under it.

Loaded lazily and failing quietly. A machine with no Radeon driver has no
atiadlxx.dll, and that is not an error -- it is a machine this module has
nothing to say about.
"""

from __future__ import annotations

import ctypes

ADL_MAX_PATH = 256
ADL_OK = 0

# ADL's VRAM vendor codes. Only the one this bench answers is named, on the
# same rule the NVAPI tables follow: an unmeasured code prints as itself
# rather than being filled in from a list that has never been checked here.
#
# Code 1 is confirmed rather than assumed. ADL reports it for this card and
# GPU-Z, reading the same VBIOS by its own route, names the card's memory
# Samsung. Two independent tools agreeing on one card is what this entry
# rests on -- which is why the rest of the list is still absent.
VRAM_VENDORS = {
    1: "Samsung",
}


MAX_PMLOG_SENSORS = 256

# ADL's PMLog sensor indices. Four of these were matched exactly against
# HWiNFO reading the same card at the same moment -- fan 1825 RPM, fan PWM
# 50%, GPU utilisation 4% and memory clock 909 MHz -- which is what says the
# enumeration is aligned rather than merely plausible. The rest are the
# neighbouring entries of the same enum, and their values agreed to within
# the degree ADL rounds to.
#
# Twenty sensors is all this card exposes through ADL, and that was tested
# rather than assumed. ADL2_Adapter_PMLog_Support_Get names exactly these
# twenty, and starting a full PMLog logging session returned the same twenty,
# so the session is not used: it is a stateful call that buys nothing over
# the plain query.
#
# Eleven of those twenty were left unread for a while, and board power was
# declared unreachable without ever being printed. It was index 73 the whole
# time. A GPU load settled it: 37 W at rest, then pinned at 361-371 W for two
# minutes at full utilisation -- which is HWiNFO's 363 W TBP limit, because a
# saturated card sits on its limit. A limit reading would not have moved.
#
# The same load confirmed the two rows that were flagged here as the ones to
# doubt. Edge and hotspot both read 31 at rest, so nothing separated them.
# Under load edge went to 56 and hotspot to 92 -- a rise of 25 against 61.
# The hotspot outruns the edge by better than two to one, which is the shape
# that says these labels are the right way round.
#
# Still not reachable, and checked rather than assumed: the TBP limit itself
# and the GPU fabric clock. Of the remaining unmapped indices, 38, 39 and
# 46-49 read zero throughout, and 40, 41 and 58 never moved off 4, 16 and 5
# across the whole idle-load-idle round trip. None of them is the 363 W limit
# or the 2401 MHz fabric clock HWiNFO shows. Those two come from ADLX, the
# newer library, present here as amdadlx64.dll and a different interface
# again. Overdrive6_CurrentPower_Get answers 0 W: this card is Overdrive 8.
#
# (index, label, unit, scale). Scale turns ADL's integer into the unit.
PMLOG_SENSORS = (
    (1, "GPU Clock", "MHz", 1.0),
    (2, "GPU Memory Clock", "MHz", 1.0),
    (8, "GPU Temperature", "°C", 1.0),
    (9, "GPU Memory Junction", "°C", 1.0),
    (14, "GPU Fan", "RPM", 1.0),
    (15, "GPU Fan PWM", "%", 1.0),
    (19, "GPU Utilization", "%", 1.0),
    (21, "GPU Core Voltage", "V", 0.001),
    (27, "GPU Hot Spot", "°C", 1.0),
    (73, "GPU Board Power (TBP)", "W", 1.0),
)


class _SensorData(ctypes.Structure):
    _fields_ = [("supported", ctypes.c_int), ("value", ctypes.c_int)]


class _PMLogDataOutput(ctypes.Structure):
    _fields_ = [
        ("size", ctypes.c_int),
        ("sensors", _SensorData * MAX_PMLOG_SENSORS),
    ]


class _AdapterInfo(ctypes.Structure):
    _fields_ = [
        ("iSize", ctypes.c_int),
        ("iAdapterIndex", ctypes.c_int),
        ("strUDID", ctypes.c_char * ADL_MAX_PATH),
        ("iBusNumber", ctypes.c_int),
        ("iDeviceNumber", ctypes.c_int),
        ("iFunctionNumber", ctypes.c_int),
        ("iVendorID", ctypes.c_int),
        ("strAdapterName", ctypes.c_char * ADL_MAX_PATH),
        ("strDisplayName", ctypes.c_char * ADL_MAX_PATH),
        ("iPresent", ctypes.c_int),
        ("iExist", ctypes.c_int),
        ("strDriverPath", ctypes.c_char * ADL_MAX_PATH),
        ("strDriverPathExt", ctypes.c_char * ADL_MAX_PATH),
        ("strPNPString", ctypes.c_char * ADL_MAX_PATH),
        ("iOSDisplayIndex", ctypes.c_int),
    ]


class _MemoryInfoX4(ctypes.Structure):
    """ADLMemoryInfoX4. iVramVendorRevId is the field that matters here."""

    _fields_ = [
        ("iMemorySize", ctypes.c_longlong),
        ("strMemoryType", ctypes.c_char * ADL_MAX_PATH),
        ("iMemoryBandwidth", ctypes.c_longlong),
        ("iHyperMemorySize", ctypes.c_longlong),
        ("iInvisibleMemorySize", ctypes.c_longlong),
        ("iVisibleMemorySize", ctypes.c_longlong),
        ("iVramVendorRevId", ctypes.c_int),
        ("iMemoryBandwidthX2", ctypes.c_int),
        ("iMemoryType", ctypes.c_int),
    ]


_ALLOCATE = ctypes.CFUNCTYPE(ctypes.c_void_p, ctypes.c_int)


@_ALLOCATE
def _allocate(size):
    """ADL allocates through the caller, so it gets a buffer it can keep."""
    return ctypes.cast(ctypes.create_string_buffer(size), ctypes.c_void_p).value


def vram_vendor(code):
    """Name a VRAM vendor code, or print the code when it is not named."""
    if code is None:
        return None
    return VRAM_VENDORS.get(code, "Vendor %d" % code)


def _with_adapter(work, bus=None, device=None, function=None):
    """Open ADL, find the card, hand it to ``work``, and always close.

    Keyed on the PCI location when one is given, so the right adapter is
    asked on a machine with more than one. ADL lists an entry per display
    output, so several share a location and the first is enough.

    Shared because both readers need the same six calls of boilerplate, and
    the one that matters is the last: the context is destroyed in a finally,
    or a failed read leaks a driver handle every time it is retried.
    """
    try:
        adl = ctypes.CDLL("atiadlxx.dll")
    except OSError:
        return None

    context = ctypes.c_void_p()
    try:
        if adl.ADL2_Main_Control_Create(_ALLOCATE(_allocate), 1,
                                        ctypes.byref(context)) != ADL_OK:
            return None
    except Exception:
        return None

    try:
        count = ctypes.c_int()
        if adl.ADL2_Adapter_NumberOfAdapters_Get(
                context, ctypes.byref(count)) != ADL_OK or count.value <= 0:
            return None
        adapters = (_AdapterInfo * count.value)()
        if adl.ADL2_Adapter_AdapterInfo_Get(
                context, adapters,
                ctypes.sizeof(_AdapterInfo) * count.value) != ADL_OK:
            return None
        for adapter in adapters:
            if not adapter.iExist:
                continue
            if bus is not None and (adapter.iBusNumber != bus
                                    or adapter.iDeviceNumber != device
                                    or adapter.iFunctionNumber != function):
                continue
            return work(adl, context, adapter.iAdapterIndex)
    except Exception:
        return None
    finally:
        try:
            adl.ADL2_Main_Control_Destroy(context)
        except Exception:
            pass
    return None


def read_memory(bus=None, device=None, function=None):
    """Return what ADL knows about the card's memory, or {}."""
    def query(adl, context, adapter_index):
        memory = _MemoryInfoX4()
        if adl.ADL2_Adapter_MemoryInfoX4_Get(
                context, adapter_index, ctypes.byref(memory)) != ADL_OK:
            return {}
        found = {}
        kind = memory.strMemoryType.decode("ascii", "ignore").strip()
        if kind:
            found["memory_type"] = kind
        vendor = vram_vendor(memory.iVramVendorRevId)
        if vendor:
            found["memory_vendor"] = vendor
        if memory.iMemorySize > 0:
            found["memory_bytes"] = int(memory.iMemorySize)
        return found

    return _with_adapter(query, bus, device, function) or {}


def format_sensor(value, unit, scale):
    """One PMLog integer as display text, in the unit the row is named for."""
    scaled = value * scale
    if unit == "V":
        return "%.3f V" % scaled
    if unit == "%":
        return "%.0f %%" % scaled
    if unit == "RPM":
        return "%.0f RPM" % scaled
    if unit == "W":
        # Whole watts: ADL hands over an integer, and a decimal point here
        # would claim a precision the driver never gave.
        return "%.0f W" % scaled
    return "%.1f %s" % (scaled, unit)


def read_sensors(bus=None, device=None, function=None):
    """Return ``{label: display text}`` for the card's sensors, or {}.

    One query serves every row: the whole sensor block comes back in a single
    call, so nine rows cost one trip through the driver rather than nine.
    """
    found = {}

    def query(adl, context, adapter_index):
        call = getattr(adl, "ADL2_New_QueryPMLogData_Get", None)
        if call is None:
            return
        data = _PMLogDataOutput()
        if call(context, adapter_index, ctypes.byref(data)) != ADL_OK:
            return
        for index, label, unit, scale in PMLOG_SENSORS:
            sensor = data.sensors[index]
            if sensor.supported:
                found[label] = format_sensor(sensor.value, unit, scale)

    _with_adapter(query, bus, device, function)
    return found
