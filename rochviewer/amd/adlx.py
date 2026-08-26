"""The card's board power limit, read through ADLX.

ADLX is AMD's newer library, and it is a C++ object model behind a C shim
rather than the flat exports of atiadlxx. Every interface is a struct whose
first member points at a vtable, and calling a method means indexing a slot.
A wrong slot does not return a wrong number: it calls whatever function
happens to sit there. So the slot constants below were transcribed from the
SDK headers -- SDK/Include/ISystem.h and IPerformanceMonitoring.h in
GPUOpen-LibrariesAndSDKs/ADLX -- rather than recalled, and each block is
counted from its own declaration.

Only one reading is taken here, because only one is new. Everything else ADLX
offers on this card, atiadlxx already answers, and two sources for one row is
a way to disagree with yourself. See [amd_adl.py] for those.

What ADLX does *not* have, checked rather than assumed: there is no fabric
clock anywhere in its headers, and no effective-clock metric. GPU FCLK is not
reachable through this library either.

Read-only. The tuning services -- the ones that could change a limit rather
than report it -- are never asked for.
"""

from __future__ import annotations

import ctypes
import time

ADLX_OK = 0
# IADLXSystem is the one interface that is not reference-counted, so its
# vtable starts at its own methods rather than Acquire/Release/QueryInterface.
SYSTEM_GET_GPUS = 1
SYSTEM_GET_PERF_SERVICES = 9

# Every other interface begins with Acquire/Release/QueryInterface, which is
# why the useful slots start at 3 and why Release is always 1.
RELEASE = 1
LIST_SIZE = 3
LIST_BEGIN = 5
LIST_AT_GPULIST = 11
PERF_GET_SUPPORTED_GPU_METRICS = 21
SUPPORT_TOTAL_BOARD_POWER_RANGE = 22

# The limit moves only when somebody drags the slider in Adrenalin, so it is
# not worth a driver round trip every tick. Thirty seconds still notices.
CACHE_SECONDS = 30.0

_CACHE = []


def _method(obj, slot, restype, *argtypes):
    """The function pointer at ``slot`` of ``obj``'s vtable, ready to call."""
    vtable = ctypes.cast(obj, ctypes.POINTER(ctypes.c_void_p))[0]
    address = ctypes.cast(vtable, ctypes.POINTER(ctypes.c_void_p))[slot]
    return ctypes.CFUNCTYPE(restype, ctypes.c_void_p, *argtypes)(address)


def _release(obj):
    if obj:
        try:
            _method(obj, RELEASE, ctypes.c_long)(obj)
        except Exception:
            pass


def _with_gpu(work):
    """Open ADLX, find the first card and the metrics services, always close.

    Everything acquired is released in the finally, and ADLX is terminated
    there too. ADLX counts its objects and complains about orphans, and a
    failed read that leaked would otherwise do it once a tick.
    """
    try:
        adlx = ctypes.CDLL("amdadlx64.dll")
    except OSError:
        return None

    version = ctypes.c_uint64()
    try:
        if adlx.ADLXQueryFullVersion(ctypes.byref(version)) != ADLX_OK:
            return None
        system = ctypes.c_void_p()
        if adlx.ADLXInitialize(ctypes.c_uint64(version.value),
                               ctypes.byref(system)) != ADLX_OK or not system:
            return None
    except Exception:
        return None

    gpu_list = ctypes.c_void_p()
    gpu = ctypes.c_void_p()
    perf = ctypes.c_void_p()
    support = ctypes.c_void_p()
    try:
        if _method(system, SYSTEM_GET_GPUS, ctypes.c_int,
                   ctypes.POINTER(ctypes.c_void_p))(
                       system, ctypes.byref(gpu_list)) != ADLX_OK:
            return None
        if not _method(gpu_list, LIST_SIZE, ctypes.c_uint)(gpu_list):
            return None
        first = _method(gpu_list, LIST_BEGIN, ctypes.c_uint)(gpu_list)
        if _method(gpu_list, LIST_AT_GPULIST, ctypes.c_int, ctypes.c_uint,
                   ctypes.POINTER(ctypes.c_void_p))(
                       gpu_list, first, ctypes.byref(gpu)) != ADLX_OK:
            return None
        if _method(system, SYSTEM_GET_PERF_SERVICES, ctypes.c_int,
                   ctypes.POINTER(ctypes.c_void_p))(
                       system, ctypes.byref(perf)) != ADLX_OK:
            return None
        if _method(perf, PERF_GET_SUPPORTED_GPU_METRICS, ctypes.c_int,
                   ctypes.c_void_p, ctypes.POINTER(ctypes.c_void_p))(
                       perf, gpu, ctypes.byref(support)) != ADLX_OK:
            return None
        return work(support)
    except Exception:
        return None
    finally:
        for handle in (support, perf, gpu, gpu_list):
            _release(handle)
        try:
            adlx.ADLXTerminate()
        except Exception:
            pass


def board_power_limit():
    """The card's board power ceiling in watts, or None.

    ADLX reports it as the top of the board power range, which is the same
    363 W the card pins itself to under a sustained load.
    """
    def query(support):
        low, high = ctypes.c_int(), ctypes.c_int()
        status = _method(support, SUPPORT_TOTAL_BOARD_POWER_RANGE,
                         ctypes.c_int, ctypes.POINTER(ctypes.c_int),
                         ctypes.POINTER(ctypes.c_int))(
                             support, ctypes.byref(low), ctypes.byref(high))
        if status != ADLX_OK or high.value <= 0:
            return None
        return int(high.value)

    return _with_gpu(query)


def cached_limit(now=None):
    """The limit, re-read at most every CACHE_SECONDS."""
    now = time.monotonic() if now is None else now
    if _CACHE and (now - _CACHE[0][0]) < CACHE_SECONDS:
        return _CACHE[0][1]
    value = board_power_limit()
    _CACHE[:] = [(now, value)]
    return value


# "Not supplied" and "ADLX would not answer" are both absences and they mean
# opposite things: one says go and read, the other says say nothing. The same
# confusion cost a test in whea_errors, so it is spelled out here too.
_UNREAD = object()


def limit_text(watts=_UNREAD):
    """The row's text: whole watts, or an em dash when ADLX did not answer."""
    if watts is _UNREAD:
        watts = cached_limit()
    return "—" if watts is None else "%d W" % watts
