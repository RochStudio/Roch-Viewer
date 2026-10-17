//! Read-only PCIe MCFG/ECAM address resolution.
//!
//! This module only reads firmware metadata and physical PCI configuration
//! space. It deliberately contains no physical/configuration write operation.

use windows::Win32::System::SystemInformation::{FIRMWARE_TABLE_PROVIDER, GetSystemFirmwareTable};

use super::{HwError, HwResult, hw};

const ACPI_PROVIDER: u32 = u32::from_be_bytes(*b"ACPI");
const MCFG_TABLE_ID: u32 = u32::from_le_bytes(*b"MCFG");
const ACPI_HEADER_SIZE: usize = 36;
const MCFG_RESERVED_SIZE: usize = 8;
const MCFG_ENTRY_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McfgAllocation {
    pub base_address: u64,
    pub segment: u16,
    pub start_bus: u8,
    pub end_bus: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct McfgVendorProbe {
    pub allocation: McfgAllocation,
    pub config_address: u64,
    pub raw_dword: u32,
    pub vendor_id: u16,
    pub device_id: u16,
}

fn u16_at(data: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([data[offset], data[offset + 1]])
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

pub fn parse_mcfg(data: &[u8]) -> Result<Vec<McfgAllocation>, String> {
    if data.len() < ACPI_HEADER_SIZE + MCFG_RESERVED_SIZE {
        return Err("MCFG table is too short".into());
    }
    if &data[..4] != b"MCFG" {
        return Err("ACPI table signature is not MCFG".into());
    }
    let declared_length = u32_at(data, 4) as usize;
    if declared_length < ACPI_HEADER_SIZE + MCFG_RESERVED_SIZE {
        return Err("MCFG declared length is invalid".into());
    }
    if declared_length > data.len() {
        return Err("MCFG table is truncated".into());
    }
    if (declared_length - ACPI_HEADER_SIZE - MCFG_RESERVED_SIZE) % MCFG_ENTRY_SIZE != 0 {
        return Err("MCFG allocation area is misaligned".into());
    }
    let mut entries = Vec::new();
    let mut offset = ACPI_HEADER_SIZE + MCFG_RESERVED_SIZE;
    while offset + MCFG_ENTRY_SIZE <= declared_length {
        let base = u64_at(data, offset);
        let segment = u16_at(data, offset + 8);
        let start_bus = data[offset + 10];
        let end_bus = data[offset + 11];
        if start_bus <= end_bus && base != 0 && base & ((1 << 20) - 1) == 0 {
            entries.push(McfgAllocation { base_address: base, segment, start_bus, end_bus });
        }
        offset += MCFG_ENTRY_SIZE;
    }
    if entries.is_empty() {
        return Err("MCFG contains no allocation entries".into());
    }
    Ok(entries)
}

pub fn select_allocation(entries: &[McfgAllocation], segment: u16, bus: u8) -> Result<McfgAllocation, String> {
    entries
        .iter()
        .find(|e| e.segment == segment && e.start_bus <= bus && bus <= e.end_bus)
        .copied()
        .ok_or_else(|| format!("MCFG has no entry for segment {segment} bus {bus}"))
}

/// The firmware's own ECAM allocation covering one bus.
///
/// Neither an Intel nor an AMD idea: the window is described by the ACPI MCFG
/// table, which both platforms have.
pub fn default_allocation(segment: u16, bus: u8) -> Result<McfgAllocation, String> {
    let table = get_mcfg_table()?;
    select_allocation(&parse_mcfg(&table)?, segment, bus)
}

pub fn ecam_address(entry: &McfgAllocation, bus: u8, device: u8, function: u8, register: u16) -> Result<u64, String> {
    if !(entry.start_bus <= bus && bus <= entry.end_bus) {
        return Err("bus is outside the MCFG allocation".into());
    }
    if device > 31 || function > 7 {
        return Err("invalid PCI device/function".into());
    }
    if register > 0xFFF {
        return Err("invalid PCI configuration register".into());
    }
    Ok(entry.base_address
        + (((bus - entry.start_bus) as u64) << 20)
        + ((device as u64) << 15)
        + ((function as u64) << 12)
        + register as u64)
}

pub fn probe_mcfg_vendor(
    table: &[u8],
    read_dword: impl Fn(u64) -> HwResult<u32>,
    segment: u16,
    bus: u8,
) -> Result<McfgVendorProbe, String> {
    let entry = select_allocation(&parse_mcfg(table)?, segment, bus)?;
    let address = ecam_address(&entry, bus, 0, 0, 0)?;
    let raw = read_dword(address).map_err(|e| e.0)?;
    Ok(McfgVendorProbe {
        allocation: entry,
        config_address: address,
        raw_dword: raw,
        vendor_id: (raw & 0xFFFF) as u16,
        device_id: ((raw >> 16) & 0xFFFF) as u16,
    })
}

/// The raw ACPI MCFG table from firmware.
pub fn get_mcfg_table() -> Result<Vec<u8>, String> {
    if cfg!(test) {
        return Err("no firmware tables in unit tests".into());
    }
    let provider = FIRMWARE_TABLE_PROVIDER(ACPI_PROVIDER);
    // SAFETY: a size query with no buffer, then a read into a buffer of that
    // size.
    unsafe {
        let size = GetSystemFirmwareTable(provider, MCFG_TABLE_ID, None);
        if size == 0 {
            return Err(super::os_error(super::last_win32_error(), "GetSystemFirmwareTable(MCFG) failed"));
        }
        let mut buffer = vec![0u8; size as usize];
        let actual = GetSystemFirmwareTable(provider, MCFG_TABLE_ID, Some(&mut buffer));
        if actual == 0 || actual > size {
            return Err(super::os_error(super::last_win32_error(), "Reading the MCFG table failed"));
        }
        buffer.truncate(actual as usize);
        Ok(buffer)
    }
}

/// 32-bit physical access through the driver's `GetPhysLong`.
pub fn read_physical_dword(address: u64) -> HwResult<u32> {
    hw().get_phys_long(address)
}

/// One PCI configuration dword through ECAM, or an error saying why not.
pub fn pci_config_dword(entry: &McfgAllocation, bus: u8, device: u8, function: u8, register: u16) -> HwResult<u32> {
    let address = ecam_address(entry, bus, device, function, register).map_err(HwError)?;
    read_physical_dword(address)
}

#[cfg(test)]
pub(crate) fn sample_table(entries: &[(u64, u16, u8, u8)]) -> Vec<u8> {
    let length = ACPI_HEADER_SIZE + MCFG_RESERVED_SIZE + MCFG_ENTRY_SIZE * entries.len();
    let mut data = vec![0u8; length];
    data[..4].copy_from_slice(b"MCFG");
    data[4..8].copy_from_slice(&(length as u32).to_le_bytes());
    let mut offset = ACPI_HEADER_SIZE + MCFG_RESERVED_SIZE;
    for (base, segment, start, end) in entries {
        data[offset..offset + 8].copy_from_slice(&base.to_le_bytes());
        data[offset + 8..offset + 10].copy_from_slice(&segment.to_le_bytes());
        data[offset + 10] = *start;
        data[offset + 11] = *end;
        offset += MCFG_ENTRY_SIZE;
    }
    data
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_selects() {
        let table = sample_table(&[(0xE000_0000, 0, 0, 0xFF)]);
        let entries = parse_mcfg(&table).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = select_allocation(&entries, 0, 0).unwrap();
        assert_eq!(entry.base_address, 0xE000_0000);
        assert_eq!(ecam_address(&entry, 0, 0x1F, 4, 0x20).unwrap(), 0xE00F_C020);
    }

    #[test]
    fn rejects_bad_tables() {
        assert!(parse_mcfg(b"short").is_err());
        let mut table = sample_table(&[(0xE000_0000, 0, 0, 0xFF)]);
        table[0] = b'X';
        assert_eq!(parse_mcfg(&table).unwrap_err(), "ACPI table signature is not MCFG");
        let misaligned = sample_table(&[(0x1234, 0, 0, 0xFF)]);
        assert_eq!(parse_mcfg(&misaligned).unwrap_err(), "MCFG contains no allocation entries");
    }

    #[test]
    fn ecam_bounds() {
        let entry = McfgAllocation { base_address: 0xE000_0000, segment: 0, start_bus: 0, end_bus: 0x7F };
        assert!(ecam_address(&entry, 0x80, 0, 0, 0).is_err());
        assert!(ecam_address(&entry, 0, 32, 0, 0).is_err());
        assert!(ecam_address(&entry, 0, 0, 8, 0).is_err());
        assert!(ecam_address(&entry, 0, 0, 0, 0x1000).is_err());
    }

    #[test]
    fn vendor_probe() {
        let table = sample_table(&[(0xE000_0000, 0, 0, 0xFF)]);
        let probe = probe_mcfg_vendor(&table, |_| Ok(0x14D8_1022), 0, 0).unwrap();
        assert_eq!(probe.vendor_id, 0x1022);
        assert_eq!(probe.device_id, 0x14D8);
        assert_eq!(probe.config_address, 0xE000_0000);
    }
}
