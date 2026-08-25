# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This file follows ZenStates-Core and ZenTimings by irusanov
# (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
# and the bounds applied to decoded values were taken from or checked against
# that work, and the comments below say where. Copyright in those parts
# remains with their authors; this file is distributed under the same licence
# they are, which is what makes that use permitted.
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

"""Read-only AMD APOB trained-memory data support.

This test-only compatibility parser targets an observed Granite Ridge
training-result record. It has no write path. Low-level physical access is
loaded lazily only when a live APOB snapshot is requested. Commercial-source
provenance approval remains pending; see INTERNAL_TEST_ONLY.txt.
"""

import hashlib
import struct
from dataclasses import dataclass


APOB_SIGNATURE = b"APOB"
GRANITE_RIDGE_BLOCK_SIZE = 0x30
MAX_APOB_TABLE_SIZE = 4 * 1024 * 1024
MAX_FIRST_CONFIG_SIZE = 0x1000
DEFAULT_APOB_ADDRESSES = (0x0A200000, 0x09F00000, 0x04000000)

_RTT_OHMS = {1: 240, 2: 120, 3: 80, 4: 60, 5: 48, 6: 40, 7: 34}
_GROUP_ODT_OHMS = {1: 480, 2: 240, 3: 120, 4: 80, 5: 60, 6: 48, 7: 40}
_PROC_ODT_OHMS = {
    1: 480.0, 2: 240.0, 3: 160.0, 4: 120.0, 5: 96.0,
    6: 80.0, 7: 68.6, 12: 60.0, 13: 53.3, 14: 48.0,
    15: 43.6, 28: 40.0, 29: 36.9, 30: 34.3, 31: 32.0,
    60: 30.0, 61: 28.2, 62: 26.7, 63: 25.3,
}
_DRAM_DQ_DS_OHMS = {0: 34, 1: 40, 2: 48}
_DIRECT_DS_OHMS = {0: None, 30: 30, 40: 40, 60: 60, 120: 120}


@dataclass(frozen=True)
class ParsedApobTraining:
    record_offset: int
    values: dict


@dataclass(frozen=True)
class ParsedApobChannels:
    """Two records attributed to channels by the validated APOB geometry."""

    channel_a: ParsedApobTraining
    channel_b: ParsedApobTraining


@dataclass(frozen=True)
class GraniteRidgeCandidate:
    """One plausible Granite Ridge record found during a diagnostic scan."""

    record_offset: int
    raw: bytes
    values: dict


@dataclass(frozen=True)
class ApobTableDiagnostic:
    """Bounded, read-only evidence about an APOB table's training records.

    This is diagnostic output only: it enumerates every plausible record and
    the container/main-entry geometry used to find them. It never selects a
    record, and it holds no physical address.
    """

    table_size: int
    header_size: int
    sha256: str
    config_offsets: tuple
    first_offset: int
    first_size: int
    main_offset: int
    main_size: int
    scan_start: int
    scan_end: int
    candidates: tuple


def _ohms(value):
    if value is None:
        return "Off"
    number = float(value)
    text = str(int(number)) if number.is_integer() else ("%.1f" % number)
    return "%s Ω" % text


def _rtt(code):
    code = int(code)
    if code == 0:
        return "Off"
    if code not in _RTT_OHMS:
        return None
    return "RZQ/%d (%d Ω)" % (code, _RTT_OHMS[code])


def _group_odt(code):
    code = int(code)
    if code == 0:
        return "Off"
    value = _GROUP_ODT_OHMS.get(code)
    return None if value is None else _ohms(value)


def _proc_odt(code):
    code = int(code)
    if code == 0:
        return "Hi-Z"
    value = _PROC_ODT_OHMS.get(code)
    return None if value is None else _ohms(value)


def _direct_drive(code):
    code = int(code)
    if code not in _DIRECT_DS_OHMS:
        return None
    return _ohms(_DIRECT_DS_OHMS[code])


def _dram_drive(code):
    value = _DRAM_DQ_DS_OHMS.get(int(code))
    return None if value is None else _ohms(value)


def decode_granite_ridge_training_block(block):
    """Decode one validated 0x30-byte Granite Ridge APOB memory record."""
    data = bytes(block)
    if len(data) < GRANITE_RIDGE_BLOCK_SIZE:
        return None

    decoded = {
        "rtt_nom_rd": _rtt(data[0x1A]),
        "rtt_nom_wr": _rtt(data[0x1B]),
        "rtt_wr": _rtt(data[0x1C]),
        "rtt_park": _rtt(data[0x1D]),
        "rtt_park_dqs": _rtt(data[0x1E]),
        "ck_odt_a": _group_odt(data[0x08]),
        "cs_odt_a": _group_odt(data[0x09]),
        "ca_odt_a": _group_odt(data[0x0A]),
        "ck_odt_b": _group_odt(data[0x0B]),
        "cs_odt_b": _group_odt(data[0x0C]),
        "ca_odt_b": _group_odt(data[0x0D]),
        # The BIOS setting "Processor DQ drive strengths", which drives all DQ
        # and DMI IOs. Confirmed by changing it from Auto to 40 ohm and
        # rebooting: this byte went 30 -> 28, and the only other bytes in the
        # block that moved were the P0 pull up and pull down pair below, which
        # is the same setting reaching the same pins.
        "proc_dq_ds": _proc_odt(data[0x0F]),
        "proc_ca_ds": _direct_drive(data[0x11]),
        "proc_ck_ds": _direct_drive(data[0x12]),
        "proc_cs_ds": _direct_drive(data[0x13]),
        "dram_dq_ds_pu": _dram_drive(data[0x1F]),
        "dram_dq_ds_pd": _dram_drive(data[0x20]),
        "proc_odt_pu": _proc_odt(data[0x21]),
        "proc_odt_pd": _proc_odt(data[0x22]),
        "proc_dq_ds_pu": _proc_odt(data[0x23]),
        "proc_dq_ds_pd": _proc_odt(data[0x24]),
    }
    return decoded if all(value is not None for value in decoded.values()) else None


def _u32(data, offset):
    if offset < 0 or offset + 4 > len(data):
        raise ValueError("APOB dword is outside the table")
    return struct.unpack_from("<I", data, offset)[0]


def extract_first_config_entry(table):
    """Return the bounded first APOB config entry from table bytes only."""
    data = bytes(table)
    if len(data) < 0x40 or data[:4] != APOB_SIGNATURE:
        raise ValueError("APOB signature/header is invalid")
    table_size = _u32(data, 0x08)
    header_size = _u32(data, 0x0C)
    if not 0x40 <= header_size <= table_size:
        raise ValueError("APOB header size is invalid")
    if not header_size <= table_size <= min(len(data), MAX_APOB_TABLE_SIZE):
        raise ValueError("APOB table size is invalid or truncated")
    config_end = header_size - 0x20
    if config_end <= 0x30:
        raise ValueError("APOB configuration list is missing")
    first = None
    for offset in range(0x30, config_end, 4):
        entry = _u32(data, offset)
        if entry and entry + 0x10 <= table_size:
            first = entry
            break
    if first is None:
        raise ValueError("APOB contains no valid configuration entries")
    first_size = _u32(data, first + 0x0C)
    if first_size > MAX_FIRST_CONFIG_SIZE:
        raise ValueError("APOB first config exceeds diagnostic limit")
    if first_size < 0x10 or first + first_size > table_size:
        raise ValueError("APOB first config is invalid or truncated")
    return first, data[first:first + first_size]


def _plausible_granite_ridge_block(block):
    data = bytes(block)
    if len(data) < GRANITE_RIDGE_BLOCK_SIZE or data[0] == 0:
        return False
    if data[1] not in (0, 1):
        return False
    if any(value > 7 for value in data[0x08:0x0E]):
        return False
    if any(value > 7 for value in data[0x1A:0x1F]):
        return False
    if not any(data[0x1A:0x1F]):
        return False
    if any(value not in _DIRECT_DS_OHMS for value in data[0x11:0x14]):
        return False
    if any(value not in _DRAM_DQ_DS_OHMS for value in data[0x1F:0x21]):
        return False
    if any(value not in _PROC_ODT_OHMS and value != 0 for value in data[0x21:0x25]):
        return False
    return decode_granite_ridge_training_block(data) is not None


def enumerate_granite_ridge_candidates(table):
    """Return bounded container metadata and every plausible training record.

    This function accepts table bytes only.  It performs no physical access
    and deliberately does not choose between candidates.
    """
    data = bytes(table)
    if len(data) < 0x40 or data[:4] != APOB_SIGNATURE:
        raise ValueError("APOB signature/header is invalid")
    table_size = _u32(data, 0x08)
    header_size = _u32(data, 0x0C)
    if not 0x40 <= header_size <= table_size:
        raise ValueError("APOB header size is invalid")
    if not header_size <= table_size <= min(len(data), MAX_APOB_TABLE_SIZE):
        raise ValueError("APOB table size is invalid or truncated")
    data = data[:table_size]

    config_end = header_size - 0x20
    if config_end <= 0x30:
        raise ValueError("APOB configuration list is missing")
    config_offsets = []
    for offset in range(0x30, config_end, 4):
        entry = _u32(data, offset)
        if entry and entry + 0x10 <= table_size:
            config_offsets.append(entry)
    if not config_offsets:
        raise ValueError("APOB contains no valid configuration entries")

    first = config_offsets[0]
    first_size = _u32(data, first + 0x0C)
    main = first + first_size
    if first_size < 0x10 or main + 0x10 > table_size:
        raise ValueError("APOB primary configuration chain is invalid")
    if data[main] != 0x01 or data[main + 4] != 0x19:
        raise ValueError("APOB memory configuration signature is invalid")
    main_size = _u32(data, main + 0x0C)
    main_end = main + main_size
    scan_start = main + 0x30
    if main_size < 0x30 + GRANITE_RIDGE_BLOCK_SIZE or main_end > table_size:
        raise ValueError("APOB memory configuration block is invalid")

    last_start = main_end - GRANITE_RIDGE_BLOCK_SIZE
    matches = []
    for offset in range(scan_start, last_start + 1):
        block = data[offset:offset + GRANITE_RIDGE_BLOCK_SIZE]
        if _plausible_granite_ridge_block(block):
            matches.append(GraniteRidgeCandidate(
                record_offset=offset,
                raw=block,
                values=decode_granite_ridge_training_block(block),
            ))

    return ApobTableDiagnostic(
        table_size=table_size,
        header_size=header_size,
        sha256=hashlib.sha256(data).hexdigest(),
        config_offsets=tuple(config_offsets),
        first_offset=first,
        first_size=first_size,
        main_offset=main,
        main_size=main_size,
        scan_start=scan_start,
        scan_end=main_end,
        candidates=tuple(matches),
    )


CCDL_WR_MAX_RATIO = 4

# The pair that marks a per-channel frequency record, and how far past it the
# [tCCD_L, tCCD_L_WR, tCCD_L_WR2] run sits, in 16-bit elements. Taken from
# ZenTimings, which moved all three readings out of the UMC registers and on
# to this marker because some boards never program 0x50198 from the BIOS
# setting at all -- so a register that disagrees with the BIOS is not a
# misread, it is a register the firmware never wrote.
#
# The marker is the point: a BIOS cannot change it, so the run is found
# without trusting the registers to be right. On this bench it appears twice,
# once per channel, and both runs read (21, 83, 42) -- the same values the
# registers give here, which is what says adopting it changes nothing where
# the registers are already correct.
CCDL_RUN_MARKER = (0x5000, 0x00C3)
CCDL_RUN_OFFSET = 9

# Plausible ranges, the same ones ZenTimings applies. The encoding was
# derived empirically and may not hold on every AGESA, so a value outside
# these is dropped rather than displayed.
CCDL_RANGE = (8, 36)
CCDL_WR2_RANGE = (8, 70)


def find_ccdl_run(table):
    """Return ``(tCCD_L, tCCD_L_WR, tCCD_L_WR2)`` from the APOB, or None.

    Located by the record marker rather than by using the registers as an
    anchor, so it still answers on a board whose registers do not follow the
    BIOS. Every marker in the table must yield the same run: the pair appears
    once per channel, and channels that disagree mean the marker is being
    matched somewhere it does not belong.
    """
    if not table or len(table) < 6:
        return None

    found = set()
    marker = struct.pack("<2H", *CCDL_RUN_MARKER)
    limit = len(table) - len(marker)
    for offset in range(0, limit + 1, 2):
        if table[offset:offset + len(marker)] != marker:
            continue
        start = offset + CCDL_RUN_OFFSET * 2
        if start + 6 > len(table):
            continue
        found.add(struct.unpack_from("<3H", table, start))

    if len(found) != 1:
        return None
    tccdl, tccdl_wr, tccdl_wr2 = found.pop()
    if not CCDL_RANGE[0] <= tccdl <= CCDL_RANGE[1]:
        return None
    if not CCDL_WR2_RANGE[0] <= tccdl_wr2 <= CCDL_WR2_RANGE[1]:
        return None
    if not tccdl <= tccdl_wr <= CCDL_WR_MAX_RATIO * tccdl:
        return None
    return tccdl, tccdl_wr, tccdl_wr2


def find_ccdl_wr(table, tccdl, tccdl_wr2):
    """Return tCCD_L_WR from the APOB, or None when it cannot be pinned down.

    tCCD_L_WR is the one of the three tCCD_L timings that no UMC register
    holds. The APOB carries all three next to each other as 16-bit values, in
    the order tCCD_L, tCCD_L_WR, tCCD_L_WR2, so the two values that *were*
    read from registers act as an anchor: find the places where those two
    bracket a third, and the value between them is tCCD_L_WR.

    That is deliberately stricter than a bare pattern scan. Every match must
    agree, and the result must satisfy tCCD_L <= tCCD_L_WR <= 4 * tCCD_L --
    the same bound ZenTimings applies -- so a coincidental byte pattern
    elsewhere in the table produces None rather than a wrong number.
    """
    if not table or not tccdl or not tccdl_wr2:
        return None

    found = set()
    limit = len(table) - 6
    for offset in range(0, limit + 1, 2):
        first, middle, last = struct.unpack_from("<3H", table, offset)
        if first == tccdl and last == tccdl_wr2:
            found.add(middle)

    if len(found) != 1:
        return None
    value = found.pop()
    if not tccdl <= value <= CCDL_WR_MAX_RATIO * tccdl:
        return None
    return value


def parse_apob_table(table):
    """Locate one unambiguous trained-memory record in a bounded APOB table."""
    diagnostic = enumerate_granite_ridge_candidates(table)
    if len(diagnostic.candidates) == 1:
        candidate = diagnostic.candidates[0]
        return ParsedApobTraining(candidate.record_offset, candidate.values)
    if len(diagnostic.candidates) > 1:
        first = diagnostic.candidates[0]
        consensus = all(
            candidate.raw == first.raw
            and candidate.record_offset
            == first.record_offset + index * GRANITE_RIDGE_BLOCK_SIZE
            for index, candidate in enumerate(diagnostic.candidates)
        )
        if consensus:
            return ParsedApobTraining(first.record_offset, first.values)
        raise ValueError("APOB contains multiple plausible Granite Ridge training records")
    raise ValueError("No plausible Granite Ridge training record found")


def parse_apob_channel_records(table):
    """Attribute ChA/ChB only for the exact validated two-record memory block.

    The observed B850MPOWER layout has a 0x30-byte lead followed by exactly
    two non-overlapping 0x30-byte records. Candidate enumeration scans every
    byte, so requiring exactly the two boundary offsets also rejects hidden
    overlapping or misaligned candidates. The record payload has no decoded
    channel identifier, so lower=ChA and upper=ChB remains internal hardware-
    validation attribution. Other layouts remain unavailable.
    """
    diagnostic = enumerate_granite_ridge_candidates(table)
    expected_offsets = (
        diagnostic.scan_start,
        diagnostic.scan_start + GRANITE_RIDGE_BLOCK_SIZE,
    )
    actual_offsets = tuple(
        candidate.record_offset for candidate in diagnostic.candidates
    )
    if (
        diagnostic.main_size != 0x90
        or diagnostic.scan_end - diagnostic.scan_start
        != 2 * GRANITE_RIDGE_BLOCK_SIZE
        or len(diagnostic.candidates) != 2
        or actual_offsets != expected_offsets
    ):
        raise ValueError("APOB channel geometry is not the validated two-record layout")

    channel_a, channel_b = diagnostic.candidates
    return ParsedApobChannels(
        channel_a=ParsedApobTraining(channel_a.record_offset, channel_a.values),
        channel_b=ParsedApobTraining(channel_b.record_offset, channel_b.values),
    )


class GraniteRidgeApobReader:
    """Locate and decode a bounded APOB table through read-only physical I/O."""

    def __init__(self, read_dword=None, candidate_addresses=DEFAULT_APOB_ADDRESSES):
        self._read_dword = read_dword
        self._candidate_addresses = tuple(int(item) for item in candidate_addresses)
        self._port_io = None
        self.table_address = None
        self.record_address = None
        self.raw_table = b""
        self.raw_record = b""
        self.channel_values = {}
        self.channel_record_addresses = {}
        self.channel_raw_records = {}
        self.ambiguous_candidates = ()
        self.last_error = ""

    def _resolve_reader(self):
        if self._read_dword is not None:
            return self._read_dword
        from rochviewer.hardware.pci_mcfg import InpOutPhysicalAccess
        from rochviewer.amd.smn import InpOutPortIO

        self._port_io = InpOutPortIO()
        if not self._port_io.is_driver_open():
            raise OSError("InpOut driver is not open")
        self._read_dword = InpOutPhysicalAccess(
            self._port_io._dll
        ).read_dword
        return self._read_dword

    @staticmethod
    def _read_bytes(read_dword, address, size):
        if size < 0 or size > MAX_APOB_TABLE_SIZE:
            raise ValueError("physical read size is outside the APOB limit")
        start = address & ~3
        end = (address + size + 3) & ~3
        data = bytearray()
        for current in range(start, end, 4):
            value = int(read_dword(current)) & 0xFFFFFFFF
            data.extend(value.to_bytes(4, "little"))
        offset = address - start
        return bytes(data[offset:offset + size])

    def read(self):
        self.last_error = ""
        self.table_address = None
        self.record_address = None
        self.raw_table = b""
        self.raw_record = b""
        self.channel_values = {}
        self.channel_record_addresses = {}
        self.channel_raw_records = {}
        self.ambiguous_candidates = ()
        errors = []
        ambiguous = []
        try:
            read_dword = self._resolve_reader()
        except Exception as exc:
            self.last_error = "APOB physical reader unavailable: %s" % exc
            return None

        successes = []
        for address in self._candidate_addresses:
            diagnostic = None
            try:
                header = self._read_bytes(read_dword, address, 0x10)
                if header[:4] != APOB_SIGNATURE:
                    continue
                table_size = _u32(header, 0x08)
                if not 0x40 <= table_size <= MAX_APOB_TABLE_SIZE:
                    raise ValueError("declared table size is outside the APOB limit")
                table = self._read_bytes(read_dword, address, table_size)
                header_after = self._read_bytes(read_dword, address, 0x10)
                if header_after != header:
                    raise ValueError("APOB header changed during the body read")
                diagnostic = enumerate_granite_ridge_candidates(table)
                channels = None
                try:
                    channels = parse_apob_channel_records(table)
                    parsed = channels.channel_a
                except ValueError:
                    parsed = parse_apob_table(table)
                raw_record = table[
                    parsed.record_offset:
                    parsed.record_offset + GRANITE_RIDGE_BLOCK_SIZE
                ]
                successes.append((address, table, parsed, raw_record, channels))
            except Exception as exc:
                if diagnostic is not None and len(diagnostic.candidates) > 1:
                    ambiguous.append((address, diagnostic))
                errors.append("0x%08X: %s" % (address, exc))
        self.ambiguous_candidates = tuple(ambiguous)
        if len(successes) == 1:
            address, table, parsed, raw_record, channels = successes[0]
            self.table_address = address
            self.record_address = address + parsed.record_offset
            self.raw_table = table
            self.raw_record = raw_record
            if channels is not None:
                channel_items = (
                    ("cha", channels.channel_a),
                    ("chb", channels.channel_b),
                )
                self.channel_values = {
                    name: dict(item.values) for name, item in channel_items
                }
                self.channel_record_addresses = {
                    name: address + item.record_offset
                    for name, item in channel_items
                }
                self.channel_raw_records = {
                    name: table[
                        item.record_offset:
                        item.record_offset + GRANITE_RIDGE_BLOCK_SIZE
                    ]
                    for name, item in channel_items
                }
            return dict(parsed.values)
        if len(successes) > 1:
            self.last_error = (
                "APOB training data unavailable (multiple valid candidate bases: %s)"
                % ", ".join("0x%08X" % item[0] for item in successes)
            )
            return None
        detail = "; ".join(errors) if errors else "signature not found"
        self.last_error = "APOB training data unavailable (%s)" % detail
        return None
