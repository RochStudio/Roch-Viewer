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

"""Read-only AGESA version discovery from mapped physical memory.

Mirrors the ZenTimings / ZenStates approach: scan a fixed low-memory window
for the ``AGESA!V9`` / ``AGESA!BB`` markers and parse the printable version
string that follows (e.g. ``ComboAm5PI 1.3.0.0``).
"""

from __future__ import annotations

import re
from typing import Callable, Optional

AGESA_MARKERS = (b"AGESA!V9", b"AGESA!BB")
# ZenTimings scans 0x09000000 .. 0x09FFFFFF in 256 KiB chunks.
DEFAULT_SCAN_START = 0x09000000
DEFAULT_SCAN_END = 0x0A000000
DEFAULT_CHUNK_SIZE = 256 * 1024

_ALLOWED = set(b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz .-")
_VERSION_RE = re.compile(
    r"^(?:ComboAm5(?:PI)?|ComboAM5(?:PI)?|ComboAm4(?:v2)?PI|AM5|AM4)"
    r"[A-Za-z0-9 ._-]{0,40}"
    r"\d+\.\d+(?:\.\d+){0,3}[A-Za-z0-9]*$"
)


def parse_agesa_version(blob: bytes) -> str:
    """Return the first AGESA version string found in ``blob``, or \"\"."""
    if not blob:
        return ""
    for marker in AGESA_MARKERS:
        start = 0
        while True:
            index = blob.find(marker, start)
            if index < 0:
                break
            version = _extract_version(blob, index + len(marker))
            if version:
                return version
            start = index + 1
    # Fallback: some dumps expose the PI name without the AGESA! prefix.
    text = blob.decode("latin-1", errors="ignore")
    match = re.search(
        r"(ComboAm5PI\s+\d+\.\d+(?:\.\d+){0,3}[A-Za-z0-9]*)",
        text,
        flags=re.IGNORECASE,
    )
    if match:
        return match.group(1).strip()
    return ""


def _extract_version(blob: bytes, offset: int) -> str:
    i = offset
    n = len(blob)
    while i < n and blob[i] not in _ALLOWED:
        # skip NULs / noise immediately after the marker
        if blob[i] not in (0x00, 0x20, 0x09):
            # hard stop on binary garbage that is not separator whitespace
            if i - offset > 8:
                return ""
        i += 1
    begin = i
    while i < n and blob[i] in _ALLOWED:
        i += 1
    if i <= begin:
        return ""
    version = blob[begin:i].decode("ascii", errors="ignore").strip(" \0")
    if len(version) < 5 or not any(ch.isdigit() for ch in version):
        return ""
    # Prefer strings that look like AGESA PI identifiers.
    if _VERSION_RE.match(version) or "Combo" in version or "PI" in version:
        return version
    # Still accept short numeric x.y.z forms right after the marker.
    if re.match(r"^\d+\.\d+(?:\.\d+){0,3}[A-Za-z0-9]*$", version):
        return version
    return ""


def find_agesa_version(
    read_chunk: Callable[[int, int], Optional[bytes]],
    start: int = DEFAULT_SCAN_START,
    end: int = DEFAULT_SCAN_END,
    chunk_size: int = DEFAULT_CHUNK_SIZE,
) -> str:
    """Scan physical memory via ``read_chunk(addr, size) -> bytes|None``."""
    if end <= start or chunk_size <= 0:
        return ""
    addr = start
    while addr < end:
        size = min(chunk_size, end - addr)
        try:
            blob = read_chunk(addr, size)
        except Exception:
            blob = None
        if blob:
            version = parse_agesa_version(bytes(blob))
            if version:
                return version
        addr += size
    return ""


def read_agesa_version_inpout() -> str:
    """Live helper using the bundled InpOut MapPhysToLin path."""
    from rochviewer.hardware.read import read_physical_memory

    def _read(addr: int, size: int):
        return read_physical_memory(addr, size)

    return find_agesa_version(_read)
