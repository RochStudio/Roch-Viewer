// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Read-only AGESA version discovery from mapped physical memory.
//!
//! Mirrors the ZenTimings / ZenStates approach: scan a fixed low-memory
//! window for the `AGESA!V9` / `AGESA!BB` markers and parse the printable
//! version string that follows (e.g. `ComboAm5PI 1.3.0.0`).

use std::sync::LazyLock;

use regex::Regex;

pub const AGESA_MARKERS: [&[u8]; 2] = [b"AGESA!V9", b"AGESA!BB"];
// ZenTimings scans 0x09000000 .. 0x09FFFFFF in 256 KiB chunks.
pub const DEFAULT_SCAN_START: u64 = 0x0900_0000;
pub const DEFAULT_SCAN_END: u64 = 0x0A00_0000;
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;

const ALLOWED: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz .-";

fn allowed(byte: u8) -> bool {
    ALLOWED.contains(&byte)
}

static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"^(?:ComboAm5(?:PI)?|ComboAM5(?:PI)?|ComboAm4(?:v2)?PI|AM5|AM4)",
        r"[A-Za-z0-9 ._-]{0,40}",
        r"\d+\.\d+(?:\.\d+){0,3}[A-Za-z0-9]*$",
    ))
    .expect("valid version pattern")
});

static NUMERIC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+\.\d+(?:\.\d+){0,3}[A-Za-z0-9]*$").expect("valid numeric pattern"));

// Python's `\s` on a str is every character str.isspace() accepts; over the
// Latin-1 range that is these, which is one more than Unicode White_Space
// (0x1C-0x1F, the information separators).
const PY_SPACE: &str = r"\t\n\x0B\x0C\r\x1C-\x1F \x{85}\x{A0}";

static FALLBACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"(?i)(ComboAm5PI[{PY_SPACE}]+\d+\.\d+(?:\.\d+){{0,3}}[A-Za-z0-9]*)"))
        .expect("valid fallback pattern")
});

fn is_py_space(ch: char) -> bool {
    matches!(ch, '\t' | '\n' | '\x0B' | '\x0C' | '\r' | '\x1C'..='\x1F' | ' ' | '\u{85}' | '\u{A0}')
}

fn find_bytes(haystack: &[u8], needle: &[u8], start: usize) -> Option<usize> {
    if start > haystack.len() {
        return None;
    }
    haystack[start..].windows(needle.len()).position(|window| window == needle).map(|index| index + start)
}

/// The first AGESA version string found in `blob`, or "".
pub fn parse_agesa_version(blob: &[u8]) -> String {
    if blob.is_empty() {
        return String::new();
    }
    for marker in AGESA_MARKERS {
        let mut start = 0;
        while let Some(index) = find_bytes(blob, marker, start) {
            let version = extract_version(blob, index + marker.len());
            if !version.is_empty() {
                return version;
            }
            start = index + 1;
        }
    }
    // Fallback: some dumps expose the PI name without the AGESA! prefix.
    let text: String = blob.iter().map(|byte| *byte as char).collect();
    if let Some(found) = FALLBACK_RE.captures(&text).and_then(|captures| captures.get(1)) {
        return found.as_str().trim_matches(is_py_space).to_string();
    }
    String::new()
}

fn extract_version(blob: &[u8], offset: usize) -> String {
    let mut i = offset;
    let n = blob.len();
    while i < n && !allowed(blob[i]) {
        // skip NULs / noise immediately after the marker
        if !matches!(blob[i], 0x00 | 0x20 | 0x09) {
            // hard stop on binary garbage that is not separator whitespace
            if i - offset > 8 {
                return String::new();
            }
        }
        i += 1;
    }
    let begin = i;
    while i < n && allowed(blob[i]) {
        i += 1;
    }
    if i <= begin {
        return String::new();
    }
    // Every allowed byte is ASCII, so the decode is the bytes themselves.
    let version = String::from_utf8_lossy(&blob[begin..i]).trim_matches([' ', '\0']).to_string();
    if version.chars().count() < 5 || !version.chars().any(|ch| ch.is_ascii_digit()) {
        return String::new();
    }
    // Prefer strings that look like AGESA PI identifiers.
    if VERSION_RE.is_match(&version) || version.contains("Combo") || version.contains("PI") {
        return version;
    }
    // Still accept short numeric x.y.z forms right after the marker.
    if NUMERIC_RE.is_match(&version) {
        return version;
    }
    String::new()
}

/// Scan physical memory via `read_chunk(addr, size) -> Option<bytes>`.
pub fn find_agesa_version(
    mut read_chunk: impl FnMut(u64, usize) -> Option<Vec<u8>>,
    start: u64,
    end: u64,
    chunk_size: usize,
) -> String {
    if end <= start || chunk_size == 0 {
        return String::new();
    }
    let mut addr = start;
    while addr < end {
        let size = (chunk_size as u64).min(end - addr) as usize;
        if let Some(blob) = read_chunk(addr, size)
            && !blob.is_empty()
        {
            let version = parse_agesa_version(&blob);
            if !version.is_empty() {
                return version;
            }
        }
        addr += size as u64;
    }
    String::new()
}

/// Live helper using the driver's MapPhysToLin path, over the default window.
pub fn read_agesa_version_inpout() -> String {
    find_agesa_version(
        crate::hardware::read::read_physical_memory,
        DEFAULT_SCAN_START,
        DEFAULT_SCAN_END,
        DEFAULT_CHUNK_SIZE,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- AgesaParseTest (test_am5_clocks_agesa.py) ---------------------------

    #[test]
    fn parse_agesa_v9_marker() {
        let blob = b"xxxxAGESA!V9ComboAm5PI 1.3.0.0\x00junk";
        assert_eq!(parse_agesa_version(blob), "ComboAm5PI 1.3.0.0");
    }

    #[test]
    fn scan_chunks() {
        let mut payload = vec![0u8; 100];
        payload.extend_from_slice(b"AGESA!V9ComboAm5PI 1.2.0.3e");
        payload.extend_from_slice(&[0u8; 50]);
        let reader = |addr: u64, size: usize| {
            if addr == 0x0900_0000 { Some(payload.clone()) } else { Some(vec![0u8; size]) }
        };
        assert_eq!(find_agesa_version(reader, 0x0900_0000, 0x0900_1000, 256), "ComboAm5PI 1.2.0.3e");
    }

    #[test]
    fn fallback_finds_the_pi_name_without_a_marker() {
        let blob = b"\x00\x00combo am5pi\x00ComboAm5PI\xA01.2.0.3\x00";
        assert_eq!(parse_agesa_version(blob), "ComboAm5PI\u{A0}1.2.0.3");
    }

    #[test]
    fn a_failed_chunk_is_skipped() {
        let reader = |addr: u64, _size: usize| {
            if addr == 0 { None } else { Some(b"AGESA!BB1.2.3".to_vec()) }
        };
        assert_eq!(find_agesa_version(reader, 0, 0x20, 0x10), "1.2.3");
        assert_eq!(find_agesa_version(|_, _| None, 0, 0x20, 0), "");
    }
}
