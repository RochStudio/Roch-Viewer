//! Memory-controller register reads: physical memory, bit fields, and the
//! dynamic search through the MCHBAR mode-register table.
//!
//! Every reader returns `None` when a read does not work, and every caller
//! handles `None`; a machine with no driver gets a viewer full of blanks and
//! a notice saying why, not a process that dies before drawing a window.

use super::hw;

/// How a direct field read is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ReadKind {
    /// One dword.
    #[default]
    Standard,
    /// Two dwords as one 64-bit little-endian quantity, for fields that
    /// straddle the 32-bit boundary.
    Wide,
}

/// A bit field at a physical address: Python's `address` + `parameters`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FieldRead {
    pub address: u64,
    pub bit_start: u32,
    /// Negative widths read the field bit-reversed, as the Python did.
    pub bit_length: i32,
    pub kind: ReadKind,
}

impl FieldRead {
    pub const fn new(address: u64, bit_start: u32, bit_length: i32) -> Self {
        Self { address, bit_start, bit_length, kind: ReadKind::Standard }
    }

    pub const fn wide(address: u64, bit_start: u32, bit_length: i32) -> Self {
        Self { address, bit_start, bit_length, kind: ReadKind::Wide }
    }

    /// The same field at another address (channel B, another controller).
    pub const fn at(self, address: u64) -> Self {
        Self { address, ..self }
    }
}

/// Python's `dynamic_params`: search the mode-register table for an entry,
/// then read the field it points at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DynamicParams {
    pub offset_start: u64,
    pub value_to_find: u64,
    pub offset_base: u64,
    pub bit_start_dynamic: u32,
    pub bit_length_dynamic: i32,
    pub mchbar: u64,
    pub command: u64,
    pub offset2: u64,
}

/// What a row asks to have read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadSpec {
    Field(FieldRead),
    Dynamic(DynamicParams),
}

/// The limit of the dynamic search, relative to MCHBAR.
pub const DYNAMIC_SEARCH_END: u64 = 0xE800;

pub fn read_physical_memory(address: u64, size: usize) -> Option<Vec<u8>> {
    // Quietly, when there is no driver at all. The rows already show nothing
    // for a reading that did not work, and a machine without the driver would
    // otherwise print this same line for every register on every refresh.
    match hw().read_phys(address, size) {
        Ok(bytes) if bytes.len() == size => Some(bytes),
        Ok(_) => None,
        Err(error) => {
            if super::driver_error().is_none() {
                eprintln!("Error reading physical memory at 0x{address:016X}: {error}");
            }
            None
        }
    }
}

/// A little-endian physical-memory value as an integer (`size` <= 8).
pub fn read_physical_memory_int(address: u64, size: usize) -> Option<u64> {
    read_physical_memory(address, size).map(|bytes| le_value(&bytes))
}

pub fn read_dword(address: u64) -> Option<u32> {
    read_physical_memory_int(address, 4).map(|v| v as u32)
}

fn le_value(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .enumerate()
        .fold(0u64, |acc, (i, b)| acc | ((*b as u64) << (8 * i)))
}

/// Extract `bit_width` bits from `raw` starting at `bit_start`; a negative
/// width returns the field with its bits reversed.
pub fn extract_value(raw: u64, bit_start: u32, bit_width: i32) -> u64 {
    let width = bit_width.unsigned_abs();
    let mask = if width >= 64 { u64::MAX } else { (1u64 << width) - 1 };
    let shifted = if bit_start >= 64 { 0 } else { raw >> bit_start };
    let value = shifted & mask;
    if bit_width < 0 {
        reverse_bits(value, width)
    } else {
        value
    }
}

fn reverse_bits(value: u64, width: u32) -> u64 {
    if width == 0 {
        return 0;
    }
    value.reverse_bits() >> (64 - width)
}

/// Python's `extract_value_from_hex`: the four bytes must be exactly four.
pub fn extract_value_from_bytes(bytes: &[u8], bit_start: u32, bit_width: i32) -> Option<u64> {
    (bytes.len() == 4).then(|| extract_value(le_value(bytes), bit_start, bit_width))
}

/// Python's `extract_value_from_wide_hex`: exactly eight bytes.
pub fn extract_value_from_wide_bytes(bytes: &[u8], bit_start: u32, bit_width: i32) -> Option<u64> {
    (bytes.len() == WIDE_READ_BYTES).then(|| extract_value(le_value(bytes), bit_start, bit_width))
}

// An all-ones dword is not a value; it is the absence of a device.
//
// Reading unmapped MMIO succeeds and returns 0xFFFFFFFF, so a field decoded
// out of it produces a number rather than a gap: 0x2CE8 is unmapped on Core
// Ultra 200S, and the eleven drive-strength rows reading it each showed 255 --
// a plausible-looking level that no register ever stated.
//
// Scoped to a fully-set dword rather than a fully-set field, deliberately. A
// narrow field of all ones is an ordinary value -- tXSR legitimately reads
// 0x3FF out of a register holding 0x000003FF -- and only the whole dword being
// set carries the "nobody answered" meaning.
pub const ALL_ONES_DWORD: u32 = 0xFFFF_FFFF;

fn dword_is_unmapped(bytes: &[u8]) -> bool {
    bytes.len() == 4 && le_value(bytes) == ALL_ONES_DWORD as u64
}

// Some memory-controller fields straddle the 32-bit boundary. 0xE050 on Core
// Ultra 200S is the case that forced this: tWRPDEN occupies bits 27..36, so
// neither the dword at 0xE050 nor the one at 0xE054 contains it.
pub const WIDE_READ_BYTES: usize = 8;

/// Search the mode-register table for `value_to_find` with a matching
/// command, then read the field its offset byte points at.
pub fn dynamic_read_physical_memory(params: &DynamicParams) -> Option<u64> {
    let start_address = params.mchbar + params.offset_start;
    let value_to_find = params.value_to_find & 0xFF;
    let end = params.mchbar + DYNAMIC_SEARCH_END;
    let mut current = start_address;
    while current < end {
        let address = current;
        current += 4;
        let Some(data) = read_physical_memory(address, 4) else {
            continue;
        };
        let data_value = le_value(&data);
        if (data_value & 0xFF) != value_to_find {
            continue;
        }
        if ((data_value >> 22) & 0x3) != params.command {
            continue;
        }
        let offset = (data_value >> 8) & 0xFF;
        let target_address = (params.mchbar + params.offset_base + offset).wrapping_sub(params.offset2);
        let Some(target) = read_physical_memory(target_address, 4) else {
            eprintln!(
                "Warning: Failed to read target address 0x{target_address:016X} \
                 for value 0x{value_to_find:02X}"
            );
            continue;
        };
        return extract_value_from_bytes(&target, params.bit_start_dynamic, params.bit_length_dynamic);
    }
    None
}

/// Read one field: Python's `read_timing`.
pub fn read_field(field: &FieldRead) -> Option<u64> {
    match field.kind {
        ReadKind::Standard => {
            let data = read_physical_memory(field.address, 4)?;
            if dword_is_unmapped(&data) {
                return None;
            }
            extract_value_from_bytes(&data, field.bit_start, field.bit_length)
        }
        ReadKind::Wide => {
            let data = read_physical_memory(field.address, WIDE_READ_BYTES)?;
            extract_value_from_wide_bytes(&data, field.bit_start, field.bit_length)
        }
    }
}

/// Read whatever a spec asks for.
pub fn read_timing(spec: &ReadSpec) -> Option<u64> {
    match spec {
        ReadSpec::Field(field) => read_field(field),
        ReadSpec::Dynamic(params) => dynamic_read_physical_memory(params),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHardware;

    #[test]
    fn extract_matches_the_python_decoder() {
        // 0x12345678 little-endian, bits 4..12
        assert_eq!(extract_value(0x1234_5678, 4, 8), 0x67);
        assert_eq!(extract_value_from_bytes(&[0x78, 0x56, 0x34, 0x12], 4, 8), Some(0x67));
        assert_eq!(extract_value_from_bytes(&[0x78, 0x56, 0x34], 4, 8), None);
    }

    #[test]
    fn negative_width_reverses_the_field() {
        // 0b0011 over 4 bits reversed is 0b1100
        assert_eq!(extract_value(0b0011, 0, -4), 0b1100);
        assert_eq!(extract_value(0b1, 0, -1), 0b1);
    }

    #[test]
    fn wide_reads_reach_across_the_dword_boundary() {
        // tWRPDEN at bits 27..36 of the pair: 90 = 0b1011010
        let raw: u64 = 90 << 27;
        let bytes = raw.to_le_bytes();
        assert_eq!(extract_value_from_wide_bytes(&bytes, 27, 10), Some(90));
        // a dword read truncates it, which is what the wide read fixed
        assert_ne!(extract_value_from_bytes(&bytes[..4], 27, 10), Some(90));
    }

    #[test]
    fn unmapped_dword_reads_as_nothing_but_a_full_field_does_not() {
        let fake = FakeHardware::new()
            .with_dword(0x1000, 0xFFFF_FFFF)
            .with_dword(0x2000, 0x0000_03FF)
            .install();
        assert_eq!(read_field(&FieldRead::new(0x1000, 0, 8)), None);
        assert_eq!(read_field(&FieldRead::new(0x2000, 0, 10)), Some(0x3FF));
        drop(fake);
    }

    #[test]
    fn dynamic_read_finds_the_entry_then_its_target() {
        let mchbar = 0xFEDC_0000u64;
        // Entry at +0xE600: value 0x0D, offset 0x10, command 1 (bits 22..24)
        let entry = 0x0Du32 | (0x10 << 8) | (1 << 22);
        let fake = FakeHardware::new()
            .with_dword(mchbar + 0xE600, 0x0000_0001)
            .with_dword(mchbar + 0xE604, entry)
            .with_dword(mchbar + 0xE200 + 0x10, 0x0000_0A50)
            .install();
        let params = DynamicParams {
            offset_start: 0xE600,
            value_to_find: 0x0D,
            offset_base: 0xE200,
            bit_start_dynamic: 4,
            bit_length_dynamic: 8,
            mchbar,
            command: 1,
            offset2: 0,
        };
        assert_eq!(dynamic_read_physical_memory(&params), Some(0xA5));
        drop(fake);
    }

    #[test]
    fn no_driver_reads_as_none() {
        assert_eq!(read_field(&FieldRead::new(0x1000, 0, 8)), None);
    }
}
