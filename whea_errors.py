"""How many hardware errors Windows has logged, and when the last one was.

WHEA is the channel firmware and the CPU use to tell Windows that something
went wrong at the hardware level: a corrected memory error, a PCIe error, a
machine check. The count is the reading a memory tuner actually wants beside
the timings -- a kit that boots and passes a benchmark can still be quietly
correcting errors, and this is where that shows.

Read from the event log rather than from hardware. The errors themselves are
in machine-check registers this project has no path to, but Windows has
already collected them, and a count it has written down is a reading rather
than a guess.

Counted since the last boot, not for all time, because the question being
asked is about this configuration: a corrected error from three BIOS versions
ago says nothing about the settings in front of you.
"""

from __future__ import annotations

import time

WHEA_PROVIDER = "Microsoft-Windows-WHEA-Logger"
EVENT_CHANNEL = "System"

# The query costs about 40 ms, and the Telemetry window ticks once a second.
# Errors are rare enough that a stale count for a few seconds costs nothing,
# and the log does not deserve to be walked sixty times a minute.
CACHE_SECONDS = 5.0

_CACHE = []


def _boot_time():
    """Seconds since the epoch when this Windows session started, or None."""
    try:
        import wmi

        systems = wmi.WMI().Win32_OperatingSystem()
        if not systems:
            return None
        raw = str(getattr(systems[0], "LastBootUpTime", "") or "")
        if len(raw) < 14:
            return None
        stamp = time.strptime(raw[:14], "%Y%m%d%H%M%S")
        return time.mktime(stamp)
    except Exception:
        return None


def _query(since=None):
    """An XPath selecting WHEA's own events, optionally only recent ones."""
    provider = "Provider[@Name='%s']" % WHEA_PROVIDER
    if since is None:
        return "*[System[%s]]" % provider
    # TimeCreated takes milliseconds back from now, which avoids having to
    # render a timestamp in the format the log wants.
    milliseconds = max(0, int((time.time() - since) * 1000))
    return "*[System[%s and TimeCreated[timediff(@SystemTime) <= %d]]]" % (
        provider, milliseconds
    )


def count_errors(since_boot=True):
    """Return the number of WHEA events, or None when the log cannot be read.

    None rather than zero on failure. Zero is a claim that the machine is
    clean, and a log this could not open has not said that.
    """
    try:
        import win32evtlog
    except Exception:
        return None

    since = _boot_time() if since_boot else None
    try:
        handle = win32evtlog.EvtQuery(
            EVENT_CHANNEL,
            win32evtlog.EvtQueryChannelPath
            | win32evtlog.EvtQueryReverseDirection,
            _query(since),
            None,
        )
    except Exception:
        return None

    found = 0
    try:
        while True:
            events = win32evtlog.EvtNext(handle, 64)
            if not events:
                break
            found += len(events)
    except Exception:
        # A partial walk is still a lower bound, but reporting it as the
        # count would understate the machine's errors. Say nothing instead.
        return None
    return found


# "Not supplied" and "the log would not answer" are both absences, and they
# have to be told apart: one means go and read, the other means say nothing.
_UNREAD = object()


def error_text(count=_UNREAD):
    """The row's text: a count, or an em dash when the log did not answer."""
    if count is _UNREAD:
        count = cached_count()
    return "—" if count is None else str(count)


def cached_count(now=None):
    """The count, re-read at most every CACHE_SECONDS."""
    now = time.monotonic() if now is None else now
    if _CACHE and (now - _CACHE[0][0]) < CACHE_SECONDS:
        return _CACHE[0][1]
    value = count_errors()
    _CACHE[:] = [(now, value)]
    return value
