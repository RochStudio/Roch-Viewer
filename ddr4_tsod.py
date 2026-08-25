"""DDR4 on-module temperature sensors (JEDEC JC-42.4 / TSE2004av).

A DDR4 module may carry a thermal sensor next to its SPD EEPROM. It is
optional: plenty of desktop kits leave the part off the board, and a module
without one simply does not answer. That is why every reading here is either a
value the sensor returned or nothing at all - there is no modelled or inferred
temperature.

The sensor lives at 0x18 + slot on the SMBus, one address per slot, and its
ambient-temperature register is a single 16-bit word. DDR5 moved this function
into the SPD5 hub, so that generation is read elsewhere; this module is only
for DDR4.
"""

from __future__ import annotations

import threading
import time

# Ambient temperature. The other JC-42.4 registers are capability, config and
# the three limit registers, none of which this project reads.
AMBIENT_TEMPERATURE_REGISTER = 0x05

# The reading occupies the low 13 bits, in 1/16 degree steps, two's complement
# with the sign at bit 12. Bits 15-13 are the TCRIT/TUPPER/TLOWER alarm flags
# and are not part of the number.
TEMPERATURE_MASK = 0x1FFF
SIGN_BIT = 0x1000
SIGN_OFFSET = 0x2000
STEP_CELSIUS = 0.0625

# What the part can physically report. A decode outside this is not a cold or
# hot DIMM, it is a failed transfer, and is dropped rather than displayed.
MIN_CELSIUS = -40.0
MAX_CELSIUS = 125.0

# Slot index -> channel. Intel desktop boards fill the first two slot positions
# from the first channel and the next two from the second.
SLOTS_PER_CHANNEL = 2
CHANNEL_LABELS = ("a", "b")


def decode_temperature(high_byte, low_byte):
    """Decode the ambient-temperature register, or None when it cannot be one.

    The sensor answers most-significant byte first, which is the opposite of
    the SMBus word convention, so the caller passes the bytes in wire order.
    """
    try:
        raw = ((int(high_byte) & 0xFF) << 8) | (int(low_byte) & 0xFF)
    except (TypeError, ValueError):
        return None

    value = raw & TEMPERATURE_MASK
    if value & SIGN_BIT:
        value -= SIGN_OFFSET
    celsius = value * STEP_CELSIUS
    if not MIN_CELSIUS <= celsius <= MAX_CELSIUS:
        return None
    return celsius


def channel_for_address(address, base_address=0x18):
    """Return the channel letter a sensor address belongs to, or None."""
    index = int(address) - base_address
    if index < 0:
        return None
    channel = index // SLOTS_PER_CHANNEL
    if channel >= len(CHANNEL_LABELS):
        return None
    return CHANNEL_LABELS[channel]


# A temperature moves slowly and every read costs a bus transaction taken
# under a mutex that HWiNFO and friends also want, so a reading is reused for
# a moment rather than re-fetched once per row per refresh.
CACHE_SECONDS = 0.75

_STATE_LOCK = threading.Lock()
_reader = None
_addresses = None
_cached = ({}, 0.0)

# Highest reading seen per channel since the process started. A memory test
# runs for hours and the peak is what decides whether the kit is cooled well
# enough; the instantaneous value at the moment someone glances at the window
# is not that number.
#
# Not what the row displays -- see temperature_text. The telemetry window
# tracks its own maximum per row and clears it on Reset Stats; this one spans
# the process and survives that button, which is a different question and the
# reason it is still kept. A caller wanting the one on screen should read the
# window's, not this.
_peaks = {}


def _get_reader(reader_factory):
    """Build the reader once. Locating the controller cannot change at runtime."""
    global _reader
    if reader_factory is not None:
        return reader_factory()
    if _reader is None:
        from intel_pch_smbus import PchSmbusReader

        _reader = PchSmbusReader()
    return _reader


def _get_addresses(reader, rescan):
    """Scan for sensors once. Modules cannot be added without a power cycle."""
    global _addresses
    if rescan or _addresses is None:
        found = reader.responding_addresses(AMBIENT_TEMPERATURE_REGISTER)
        if rescan:
            return found
        _addresses = found
    return _addresses


def reset_cache():
    """Drop the cached controller, sensor list, reading and peaks."""
    global _reader, _addresses, _cached
    with _STATE_LOCK:
        _reader = None
        _addresses = None
        _cached = ({}, 0.0)
        _peaks.clear()


def peak_temperatures():
    """Return ``{channel letter: highest celsius seen}``."""
    return dict(_peaks)


def read_dimm_temperatures(reader_factory=None, monotonic=time.monotonic):
    """Return ``{channel letter: celsius}`` for every sensor that answered.

    Fail-closed like the other privileged transports: a slot that does not
    answer, or that answers with something outside the part's range, is left
    out of the result rather than reported as a number. When a channel holds
    two populated slots, the hotter one is kept - that is the module the
    airflow is failing and the one worth watching while tuning.
    """
    global _cached
    injected = reader_factory is not None

    if not injected:
        values, stamp = _cached
        now = monotonic()
        if values and now - stamp < CACHE_SECONDS:
            return dict(values)

    try:
        with _STATE_LOCK:
            reader = _get_reader(reader_factory)
            addresses = _get_addresses(reader, rescan=injected)
    except Exception:
        return {}

    temperatures = {}
    for address in addresses:
        channel = channel_for_address(address)
        if channel is None:
            continue
        try:
            high, low = reader.read_word_bytes(
                address, AMBIENT_TEMPERATURE_REGISTER
            )
        except Exception:
            continue
        celsius = decode_temperature(high, low)
        if celsius is None:
            continue
        previous = temperatures.get(channel)
        if previous is None or celsius > previous:
            temperatures[channel] = celsius

    for channel, celsius in temperatures.items():
        if celsius > _peaks.get(channel, float("-inf")):
            _peaks[channel] = celsius

    if not injected and temperatures:
        _cached = (dict(temperatures), monotonic())
    return temperatures


def temperature_text(channel, reader_factory=None):
    """Format one channel's DIMM temperature, or None.

    The reading alone. It used to trail its own peak -- "29.2 °C  (max 99.9)"
    -- because these rows lived on a tab, and a tab shows one instant, so the
    peak had nowhere else to go.

    They live in the telemetry window now, which gives every row a Max column
    tracking exactly that, and a button to reset it. Carrying the peak here as
    well was wrong twice over: nineteen characters in a column sized for
    seven, so the text overflowed across the Min and Max cells beside it; and
    a second maximum, kept in this module, that Reset Stats could not clear
    and that would drift from the one on screen the moment it was pressed.
    """
    celsius = read_dimm_temperatures(reader_factory=reader_factory).get(channel)
    return None if celsius is None else f"{celsius:.1f} °C"
