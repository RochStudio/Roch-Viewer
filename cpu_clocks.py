"""Live CPU core clocks, without a driver.

The clocks a tuner watches -- BCLK, ring, MCLK -- all come out of MCHBAR and
are already read. Core clock is the one that does not: the P-state ratio lives
in IA32_PERF_STATUS, and this project's driver maps physical memory and I/O
ports but has no RDMSR. So the number HWiNFO shows next to each core is not
reachable the way HWiNFO reaches it.

Two routes were measured before this one was chosen:

  CallNtPowerInformation  0.013 ms, and useless. CurrentMhz reads 3187 for
                          every logical processor on this bench -- the nominal
                          frequency, not the running one -- while the cores
                          are at 5.7 GHz. A constant that looks like a reading.
  WMI perf class          real numbers, 452 ms per query. The telemetry window
                          ticks once a second; half a second of that on one
                          group is not a cost this project pays.
  PDH                     the same counters WMI wraps, read natively: 0.12 ms.

So this is PDH. What it gives is an average over the sampling interval rather
than an instantaneous ratio, which is why it reads a little under HWiNFO --
5436-5671 MHz per thread against HWiNFO's 5700 while it was open beside it.
That is a different measurement, not a worse one, and the rows are named for
what it is.

SAFETY - read-only, and no driver involved: performance counters any user can
read, through PdhAddEnglishCounter so the paths do not depend on the display
language.
"""

import ctypes
import threading
import time
from ctypes import wintypes

PDH_FMT_DOUBLE = 0x00000200
PDH_MORE_DATA = 0x800007D2
PDH_CSTATUS_VALID_DATA = 0x00000000
PDH_CSTATUS_NEW_DATA = 0x00000001

# "Processor Information" rather than "Processor": the older object does not
# break out processor groups, and reports nothing useful above 64 threads.
PERFORMANCE_COUNTER = r"\Processor Information(*)\% Processor Performance"
UTILITY_COUNTER = r"\Processor Information(*)\% Processor Utility"
FREQUENCY_COUNTER = r"\Processor Information(_Total)\Processor Frequency"

# Instances PDH adds on top of the per-thread ones. "_Total" is the machine
# and "0,_Total" is processor group 0; both are aggregates and neither is a
# core, so a maximum taken across instances has to exclude them.
TOTAL_INSTANCE_SUFFIX = "_Total"

# A performance ratio is a percentage of nominal, and turbo puts it above 100.
# Past this it is not a clock, it is a counter that wrapped.
MAX_PERFORMANCE_PERCENT = 1000.0

# The nominal frequency is megahertz, not a percentage, and needs its own
# bound. Sharing the percentage one rejected a perfectly good 3187 MHz and
# left every clock row blank.
MAX_NOMINAL_MHZ = 100000.0

# One collection serves every row that asks within this window.
#
# These are rate counters: each collection is measured against the one before
# it, so two taken microseconds apart divide by almost nothing. The three
# clock rows read in the same tick did exactly that, and the second and third
# came back at 7410 MHz -- the first row's real 5675 MHz, then noise. Sharing
# one collection across the tick is what makes them the same measurement.
MIN_SAMPLE_INTERVAL_S = 0.25


class _Pdh:
    """One open PDH query, kept for the life of the process.

    Opening a query and adding counters costs more than collecting from one,
    and a rate counter needs two collections before it means anything, so a
    query per tick would never produce a reading at all.
    """

    def __init__(self, monotonic=time.monotonic):
        self._lock = threading.Lock()
        self._monotonic = monotonic
        self._pdh = None
        self._query = None
        self._counters = {}
        self._primed = False
        self._failed = False
        self._sampled_at = None
        self._sample = {}

    def _open(self):
        if self._pdh is not None or self._failed:
            return self._pdh is not None
        try:
            pdh = ctypes.WinDLL("pdh")
            query = wintypes.HANDLE()
            if pdh.PdhOpenQueryW(None, 0, ctypes.byref(query)):
                raise OSError("PdhOpenQuery failed")
            counters = {}
            for key, path in (("performance", PERFORMANCE_COUNTER),
                              ("utility", UTILITY_COUNTER),
                              ("frequency", FREQUENCY_COUNTER)):
                handle = wintypes.HANDLE()
                if pdh.PdhAddEnglishCounterW(query, path, 0,
                                             ctypes.byref(handle)):
                    continue
                counters[key] = handle
            if "performance" not in counters:
                raise OSError("no processor performance counter")
            pdh.PdhCollectQueryData(query)
            self._pdh, self._query, self._counters = pdh, query, counters
            return True
        except Exception:
            self._failed = True
            return False

    def collect(self):
        """Return ``{key: [(instance, value)]}``, or {} when unavailable."""
        with self._lock:
            if not self._open():
                return {}
            now = self._monotonic()
            if (self._sampled_at is not None
                    and now - self._sampled_at < MIN_SAMPLE_INTERVAL_S):
                return self._sample
            try:
                if self._pdh.PdhCollectQueryData(self._query):
                    return {}
                # The first collection after opening establishes the baseline
                # a rate counter is measured against; it has no value yet.
                #
                # Stamped as though it were a sample, so the interval guard
                # above holds the next collection off until a real interval
                # has passed. Without that, the row after this one collected
                # microseconds later and read 5834 MHz -- and the telemetry
                # window keeps maxima for the session, so one spurious first
                # sample would have sat in the Max column until it was reset.
                if not self._primed:
                    self._primed = True
                    self._sampled_at = now
                    self._sample = {}
                    return {}
                self._sample = {
                    key: self._array(handle)
                    for key, handle in self._counters.items()
                }
                self._sampled_at = now
                return self._sample
            except Exception:
                return {}

    def _array(self, counter):
        size = wintypes.DWORD(0)
        count = wintypes.DWORD(0)
        status = self._pdh.PdhGetFormattedCounterArrayW(
            counter, PDH_FMT_DOUBLE, ctypes.byref(size), ctypes.byref(count),
            None,
        )
        if status & 0xFFFFFFFF != PDH_MORE_DATA or not count.value:
            return []
        buffer = ctypes.create_string_buffer(size.value)
        if self._pdh.PdhGetFormattedCounterArrayW(
            counter, PDH_FMT_DOUBLE, ctypes.byref(size), ctypes.byref(count),
            ctypes.byref(buffer),
        ):
            return []
        items = ctypes.cast(
            buffer, ctypes.POINTER(_CounterItem * count.value)
        ).contents
        return [
            (item.szName, item.value.double)
            for item in items
            if item.status in (PDH_CSTATUS_VALID_DATA, PDH_CSTATUS_NEW_DATA)
        ]


class _CounterValue(ctypes.Union):
    _fields_ = [
        ("long", wintypes.LONG),
        ("double", ctypes.c_double),
        ("large", ctypes.c_longlong),
        ("ansi", ctypes.c_char_p),
        ("wide", ctypes.c_wchar_p),
    ]


class _CounterItem(ctypes.Structure):
    # PDH_FMT_COUNTERVALUE_ITEM_W. The union is eight-byte aligned, so the
    # status word is followed by padding the layout has to name explicitly.
    _fields_ = [
        ("szName", ctypes.c_wchar_p),
        ("status", wintypes.DWORD),
        ("_padding", wintypes.DWORD),
        ("value", _CounterValue),
    ]


_QUERY = _Pdh()


def _per_core(rows):
    """Drop the aggregate instances, leaving one entry per logical processor."""
    return [
        value for name, value in rows
        if not str(name).endswith(TOTAL_INSTANCE_SUFFIX)
        and 0.0 <= value <= MAX_PERFORMANCE_PERCENT
    ]


def _total(rows, maximum=MAX_PERFORMANCE_PERCENT):
    """The machine-wide instance, which PDH names "_Total" exactly.

    The bound is a parameter because these rows are not all percentages: the
    nominal frequency comes through the same shape in megahertz.
    """
    for name, value in rows:
        if str(name) == TOTAL_INSTANCE_SUFFIX:
            return value if 0.0 <= value <= maximum else None
    return None


def _instance_order(name):
    """Sort key for a PDH processor instance.

    They are named "group,processor" -- "0,0", "0,1", "0,10" -- which sorts
    wrong as text: "0,10" lands between "0,1" and "0,2", so a list of cores
    ordered by name is not ordered by core.
    """
    try:
        return tuple(int(part) for part in str(name).split(","))
    except ValueError:
        return (1 << 30,)


def processor_label(position):
    """Name one logical processor by its place in the ordered list.

    Logical processor, not core: which of them share a physical core, and
    which are performance or efficiency cores, is topology this has no way to
    read. A plain number claims nothing extra.

    By position rather than by the number inside the instance name, because
    that number restarts in each processor group -- a machine with two groups
    has an instance "0,0" and an instance "1,0", and naming both "CPU 0"
    would collide.
    """
    return "CPU %d" % position


def core_count():
    """How many logical processors to expect, without reading a counter.

    The rows are built at import, and the counters cannot answer then: these
    are rates, so the first collection only establishes a baseline and the
    real one comes an interval later. Asking the counters for the row list
    returned nothing and the breakdown was simply absent.

    Windows' own count is the same set PDH enumerates, and needs no sample.
    """
    try:
        import os

        return int(os.cpu_count() or 0)
    except Exception:
        return 0


def read_core_clocks(collect=None):
    """[(label, megahertz)] effective clock per logical processor, or [].

    Shares a collection with read_clocks through the interval cache, so a
    parent row and its expanded children all describe the same sample.
    """
    rows = (collect or _QUERY.collect)()
    utility = rows.get("utility") or []
    if not utility:
        return []
    nominal = _total(rows.get("frequency") or [], MAX_NOMINAL_MHZ)
    if not nominal:
        return []
    cores = [
        (name, value) for name, value in utility
        if not str(name).endswith(TOTAL_INSTANCE_SUFFIX)
        and 0.0 <= value <= MAX_PERFORMANCE_PERCENT
    ]
    cores.sort(key=lambda entry: _instance_order(entry[0]))
    return [
        (processor_label(position), nominal * value / 100.0)
        for position, (_name, value) in enumerate(cores)
    ]


def core_clock_text(index):
    """One logical processor's effective clock, or None."""
    try:
        cores = read_core_clocks()
    except Exception:
        return None
    if not 0 <= index < len(cores):
        return None
    return "%.0f Mhz" % cores[index][1]


def core_labels():
    """The logical processors to draw rows for, in order.

    From the count rather than from a reading; see core_count. A processor
    the counters turn out not to report reads nothing, which is the same
    thing every unreadable row in this project does.
    """
    return [processor_label(position) for position in range(core_count())]


def read_clocks(collect=None):
    """Return ``{key: megahertz}`` for the core clocks, or {}.

    ``core_avg``       what the busy cores averaged over the interval.
    ``core_max``       the fastest single logical processor.
    ``core_effective`` the same scale with idle time counted, which is the
                       quantity HWiNFO calls the effective clock.
    """
    rows = (collect or _QUERY.collect)()
    performance = rows.get("performance") or []
    if not performance:
        return {}
    nominal = _total(rows.get("frequency") or [], MAX_NOMINAL_MHZ)
    if not nominal:
        return {}

    found = {}
    total = _total(performance)
    if total is not None:
        found["core_avg"] = nominal * total / 100.0
    cores = _per_core(performance)
    if cores:
        found["core_max"] = nominal * max(cores) / 100.0
    utility = _total(rows.get("utility") or [])
    if utility is not None:
        found["core_effective"] = nominal * utility / 100.0
    return found


def clock_text(key):
    """One clock, written the way the other clock rows are, or None."""
    try:
        megahertz = read_clocks().get(key)
    except Exception:
        return None
    return None if megahertz is None else "%.0f Mhz" % megahertz
