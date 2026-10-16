//! DDR4 SPD identity, read from the module's EE1004 EEPROM.
//!
//! SMBIOS carries no build date and may omit the serial and DRAM vendor, so
//! these rows need the module's own SPD. On DDR5 that is
//! [`ddr5_spd`](super::ddr5_spd); this is the DDR4 half.
//!
//! Nothing here can write EEPROM data. DDR4 page selection does require the
//! standard volatile EE1004 Send Byte command at fixed SPA0/SPA1 addresses;
//! the transport exposes that selector directly and never accepts an EEPROM
//! write.
//!
//! A DDR4 module's SPD is 512 bytes exposed 256 at a time, and the
//! manufacturing block -- bytes 320 to 352 -- is in the upper half. The reader
//! selects page 1, reads the identity bytes while holding the shared SMBus
//! mutex, then restores page 0 for the next tool.
//!
//! The proof is the part number. Bytes 329-348 are twenty ASCII characters,
//! and on the Z790-P bench registers 0x49-0x5C read "F4-3600C14-16GVKA" --
//! which is the part number SMBIOS reports for the same stick, and which lands
//! at that offset only if the window base is byte 256. The JEP106 pairs on
//! either side agree: 0x04CD at register 0x40 is G.Skill, the module maker,
//! and 0x80CE at 0x5E is Samsung, the DRAM maker. Three fields from three
//! different parts of the block, all landing where the upper half puts them.
//!
//! If that check fails because the bus is unreadable, this reports nothing and
//! the rows fall back to SMBIOS. It never decodes a block it has not first
//! confirmed from the part number.
//!
//! Optional bytes may be unprogrammed even when the read succeeded. The
//! original G.Skill bench kit has zeros across that optional block. The Acer
//! BL.9BWWR.298 kit used for the 1.0.5 fix supplies week 10 of 2022 and
//! Samsung stepping 0x42, while its serial is zero; firmware and the reference
//! display it state as `0`.

use std::collections::BTreeMap;

pub use super::ddr5_spd::EM_DASH;
use super::ddr5_spd::{SpdIdentity, decode_jep106_id, decode_manufacture_date};
use super::ddr5_telemetry::{ReaderFactory, SmbusReader, SpdBytes, default_smbus_backend};
use super::spd_profiles::{SpdRecord, SpdValue, decode_ddr4_spd};
use crate::memo::Memo;

pub const SPD_MODULE_MFG_ID: u32 = 0x40; // byte 320, 2 bytes, JEP106
pub const SPD_MFG_LOCATION: u32 = 0x42; // byte 322
pub const SPD_MFG_YEAR: u32 = 0x43; // byte 323, BCD, years since 2000
pub const SPD_MFG_WEEK: u32 = 0x44; // byte 324, BCD, ISO week
pub const SPD_SERIAL_NUMBER: u32 = 0x45; // byte 325, 4 bytes
pub const SPD_SERIAL_LENGTH: u32 = 4;
pub const SPD_PART_NUMBER: u32 = 0x49; // byte 329, 20 ASCII bytes
pub const SPD_PART_NUMBER_LENGTH: u32 = 20;
pub const SPD_MODULE_REVISION: u32 = 0x5D; // byte 349
pub const SPD_DRAM_MFG_ID: u32 = 0x5E; // byte 350, 2 bytes, JEP106
pub const SPD_DRAM_STEPPING: u32 = 0x60; // byte 352

// The lower half puts the DRAM device type here. Reading it confirms the
// negative case as directly as the part number confirms the positive one:
// 0x0C at register 0x02 means the window is on the lower half and the
// manufacturing block is not reachable without a write.
pub const SPD_DEVICE_TYPE: u32 = 0x02;
pub const SPD_DEVICE_TYPE_DDR4: u8 = 0x0C;

// A part number is the alignment proof, so it has to look like one: printable,
// and long enough that a run of stray bytes cannot pass for it.
pub const MINIMUM_PART_NUMBER: usize = 4;

// Which hubs answered last, so repeat reads skip the bus scan.
static CACHE: Memo<Vec<SpdIdentity>> = Memo::new();
static MODULE_CACHE: Memo<Vec<SpdRecord>> = Memo::new();

/// The twenty ASCII bytes, or "" when they are not ASCII at all.
pub fn decode_part_number(values: &SpdBytes) -> String {
    let mut characters = String::new();
    for offset in SPD_PART_NUMBER..SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH {
        let Some(&byte) = values.get(&offset) else {
            continue;
        };
        // Unprogrammed tail bytes are 0x00 or 0xFF and end the string rather
        // than corrupting it; anything else non-printable means this is not a
        // part number and the caller should not trust the block.
        if byte == 0x00 || byte == 0xFF {
            continue;
        }
        if !(0x20..0x7F).contains(&byte) {
            return String::new();
        }
        characters.push(byte as char);
    }
    characters.trim().to_string()
}

/// The serial as eight hex digits, `0`, or an em dash.
///
/// Some DDR4 modules leave all four bytes zero. Firmware and the reference
/// expose that state as serial `0`, so preserve it rather than making a
/// successful SPD read look unavailable. An all-FF block is still an
/// erased/unreadable value.
pub fn decode_serial_number(values: &SpdBytes) -> String {
    let mut digits = String::new();
    for offset in SPD_SERIAL_NUMBER..SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH {
        let Some(byte) = values.get(&offset) else {
            return EM_DASH.to_string();
        };
        digits.push_str(&format!("{byte:02X}"));
    }
    if digits.trim_matches('0').is_empty() {
        return "0".to_string();
    }
    if digits.trim_matches('F').is_empty() {
        return EM_DASH.to_string();
    }
    digits
}

/// Decode one manufacturing block, or `None` when it is not one.
///
/// `None` means the alignment check failed, which is the only thing standing
/// between this and decoding the lower half of the SPD as though it were the
/// upper one.
pub fn decode_identity(values: &SpdBytes) -> Option<SpdIdentity> {
    let part_number = decode_part_number(values);
    if part_number.chars().count() < MINIMUM_PART_NUMBER {
        return None;
    }
    let byte = |offset: u32| values.get(&offset).copied();
    Some(SpdIdentity {
        part_number,
        module_manufacturer: decode_jep106_id(byte(SPD_MODULE_MFG_ID), byte(SPD_MODULE_MFG_ID + 1)),
        serial_number: decode_serial_number(values),
        manufacture_date: decode_manufacture_date(byte(SPD_MFG_YEAR), byte(SPD_MFG_WEEK)),
        dram_manufacturer: decode_jep106_id(byte(SPD_DRAM_MFG_ID), byte(SPD_DRAM_MFG_ID + 1)),
        dram_stepping: byte(SPD_DRAM_STEPPING),
        dram_die: None,
        address: 0,
        controller: 0,
    })
}

/// Registers needed from DDR4's upper manufacturing page. The transport reads
/// their contiguous range after selecting EE1004 page 1.
pub const IDENTITY_REGISTERS: [u32; 33] = {
    let mut out = [0u32; 33];
    let mut n = 0;
    let mut r = SPD_MODULE_MFG_ID;
    while r < SPD_MODULE_MFG_ID + 2 {
        out[n] = r;
        n += 1;
        r += 1;
    }
    out[n] = SPD_MFG_LOCATION;
    out[n + 1] = SPD_MFG_YEAR;
    out[n + 2] = SPD_MFG_WEEK;
    n += 3;
    r = SPD_SERIAL_NUMBER;
    while r < SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH {
        out[n] = r;
        n += 1;
        r += 1;
    }
    r = SPD_PART_NUMBER;
    while r < SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH {
        out[n] = r;
        n += 1;
        r += 1;
    }
    out[n] = SPD_MODULE_REVISION;
    n += 1;
    r = SPD_DRAM_MFG_ID;
    while r < SPD_DRAM_MFG_ID + 2 {
        out[n] = r;
        n += 1;
        r += 1;
    }
    out[n] = SPD_DRAM_STEPPING;
    out
};

/// The transport for the DDR4 readers: the caller's reader with the
/// platform's addresses, or the platform's reader. `None` when there is
/// neither.
fn resolve_transport(
    reader_factory: Option<ReaderFactory<'_>>,
) -> Option<(ReaderFactoryOwned<'_>, &'static [u8], &'static [u8])> {
    match default_smbus_backend() {
        None => {
            let factory = reader_factory?;
            // See the note in ddr5_spd::read_identity: a caller that supplied
            // a reader supplied the transport, and the addresses are JEDEC's
            // rather than the platform's.
            use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES};
            Some((ReaderFactoryOwned::Borrowed(factory), &CONTROLLER_OFFSETS, &SPD_HUB_ADDRESSES))
        }
        Some(backend) => {
            let factory = match reader_factory {
                Some(factory) => ReaderFactoryOwned::Borrowed(factory),
                None => ReaderFactoryOwned::Platform(backend.factory),
            };
            Some((factory, backend.controllers, backend.hubs))
        }
    }
}

/// A reader factory either handed in or taken from the platform backend.
enum ReaderFactoryOwned<'a> {
    Borrowed(ReaderFactory<'a>),
    Platform(fn() -> Result<std::sync::Arc<dyn SmbusReader>, super::ddr5_telemetry::SmbusError>),
}

impl ReaderFactoryOwned<'_> {
    fn build(&self) -> Result<std::sync::Arc<dyn SmbusReader>, super::ddr5_telemetry::SmbusError> {
        match self {
            ReaderFactoryOwned::Borrowed(factory) => factory(),
            ReaderFactoryOwned::Platform(factory) => factory(),
        }
    }
}

/// One identity per DDR4 module that answers, or none.
///
/// Cached like its DDR5 counterpart: the System Info rows read it at startup
/// and the bus scan is the slow part.
pub fn read_identity(reader_factory: Option<ReaderFactory>, refresh: bool) -> Vec<SpdIdentity> {
    let use_cache = reader_factory.is_none();
    if use_cache
        && !refresh
        && let Some(cached) = CACHE.get()
    {
        return cached;
    }

    let Some((factory, controllers, hub_addresses)) = resolve_transport(reader_factory) else {
        return Vec::new();
    };
    let run = || -> Result<Vec<SpdIdentity>, String> {
        let mut modules = Vec::new();
        let reader = factory.build().map_err(|e| e.to_string())?;
        if reader.is_driver_open() {
            for &controller in controllers {
                for &address in hub_addresses {
                    if let Some(identity) = read_one(reader.as_ref(), address, controller)? {
                        modules.push(identity);
                    }
                }
                // Modules live on one controller; once it answered, stop
                // probing the other, since probing an empty address blocks.
                if !modules.is_empty() {
                    break;
                }
            }
        }
        Ok(modules)
    };
    let modules = run().unwrap_or_else(|exc| {
        eprintln!("Error reading DDR4 SPD identity: {exc}");
        Vec::new()
    });

    if use_cache {
        CACHE.set(modules.clone());
    }
    modules
}

/// One module's identity, or `None` when this address has nothing to say.
fn read_one(reader: &dyn SmbusReader, address: u8, controller: u8) -> Result<Option<SpdIdentity>, String> {
    if !reader.probe_address(address, controller) {
        return Ok(None);
    }
    let start = 0x100 + IDENTITY_REGISTERS.iter().min().copied().unwrap_or(0);
    let end = 0x100 + IDENTITY_REGISTERS.iter().max().copied().unwrap_or(0) + 1;
    let absolute = match reader.read_ddr4_spd(address, start as i64, (end - start) as i64, controller) {
        Ok(absolute) => absolute,
        Err(error) if error.is_transfer() => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let values: SpdBytes = absolute.into_iter().map(|(position, value)| (position - 0x100, value)).collect();
    let Some(mut identity) = decode_identity(&values) else {
        return Ok(None);
    };
    identity.address = address;
    identity.controller = controller;
    Ok(Some(identity))
}

/// Complete, decoded DDR4 SPD records for the SPD tab.
///
/// The base timing section and XMP user area are read through the existing
/// EE1004 page-aware transport.  No EEPROM data is written; only the volatile
/// page selector used by every DDR4 SPD reader changes while a page is read.
pub fn read_modules(reader_factory: Option<ReaderFactory>, refresh: bool) -> Vec<SpdRecord> {
    let use_cache = reader_factory.is_none();
    if use_cache
        && !refresh
        && let Some(cached) = MODULE_CACHE.get()
    {
        return cached;
    }

    let Some((factory, controllers, hub_addresses)) = resolve_transport(reader_factory) else {
        return Vec::new();
    };
    let run = || -> Result<Vec<SpdRecord>, String> {
        let reader = factory.build().map_err(|e| e.to_string())?;
        if !reader.is_driver_open() {
            return Ok(Vec::new());
        }
        let identities = read_identity(reader_factory, refresh);
        collect_modules(reader.as_ref(), controllers, hub_addresses, &identities)
    };
    let modules = run().unwrap_or_else(|exc| {
        eprintln!("Error reading DDR4 SPD profiles: {exc}");
        Vec::new()
    });

    if use_cache {
        MODULE_CACHE.set(modules.clone());
    }
    modules
}

/// Read and decode every module the identity pass found.
pub(crate) fn collect_modules(
    reader: &dyn SmbusReader,
    controllers: &[u8],
    hub_addresses: &[u8],
    identities: &[SpdIdentity],
) -> Result<Vec<SpdRecord>, String> {
    let identities: BTreeMap<(u8, u8), &SpdIdentity> =
        identities.iter().map(|item| ((item.controller, item.address), item)).collect();
    let mut modules = Vec::new();
    for &controller in controllers {
        for &address in hub_addresses {
            let Some(identity) = identities.get(&(controller, address)) else {
                continue;
            };
            let mut values = SpdBytes::new();
            // Read only bytes the display decodes. A Byte Data SMBus
            // transaction is issued for every position, so skipping the
            // reserved areas cuts the first-open wait sharply.
            for (start, length) in [(2, 28), (120, 6), (384, 6)] {
                values.extend(reader.read_ddr4_spd(address, start, length, controller).map_err(|e| e.to_string())?);
            }
            let enabled = values.get(&386).copied().unwrap_or(0);
            let profile_ranges = [[(393, 14), (427, 5)], [(440, 14), (474, 5)]];
            for (index, ranges) in profile_ranges.iter().enumerate() {
                if enabled & (1 << index) == 0 {
                    continue;
                }
                for &(start, length) in ranges {
                    values.extend(reader.read_ddr4_spd(address, start, length, controller).map_err(|e| e.to_string())?);
                }
            }
            if let Some(mut decoded) = decode_ddr4_spd(&values, Some(&identity.fields()))? {
                decoded.set("address", SpdValue::Int(address as i64));
                decoded.set("controller", SpdValue::Int(controller as i64));
                modules.push(decoded);
            }
        }
        if !modules.is_empty() {
            break;
        }
    }
    Ok(modules)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ddr5_telemetry::SmbusError;
    use std::sync::{Arc, Mutex};

    // Captured from the Z790-P bench (G.Skill F4-3600C14-16GVKA, EEPROMs 0x51
    // and 0x53, both identical): module vendor 04 CD at register 0x40, then a
    // location, date and serial that G.Skill left unprogrammed, the part
    // number at 0x49, a zero revision, DRAM vendor 80 CE and a zero stepping.
    fn bench_block() -> Vec<u8> {
        let mut block = vec![0x04, 0xCD, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00];
        let mut part = b"F4-3600C14-16GVKA".to_vec();
        part.resize(SPD_PART_NUMBER_LENGTH as usize, 0x00);
        block.extend(part);
        block.extend([0x00, 0x80, 0xCE, 0x00, 0x00]);
        block
    }

    /// The bench block with overrides; `None` removes a byte, as a read that
    /// did not answer leaves it out.
    fn bench_values(overrides: &[(u32, Option<u8>)]) -> SpdBytes {
        let mut values: SpdBytes =
            bench_block().into_iter().enumerate().map(|(i, b)| (SPD_MODULE_MFG_ID + i as u32, b)).collect();
        for (offset, value) in overrides {
            match value {
                Some(value) => values.insert(*offset, *value),
                None => values.remove(offset),
            };
        }
        values
    }

    // --- PartNumberTest: the part number is what proves which half of the
    // SPD is in the window.

    #[test]
    fn the_bench_part_number_decodes() {
        assert_eq!(decode_part_number(&bench_values(&[])), "F4-3600C14-16GVKA");
    }

    #[test]
    fn unprogrammed_tail_bytes_end_the_string() {
        // 0x00 and 0xFF are padding, not corruption: they stop the string
        // rather than disqualifying the block.
        let values = bench_values(&[(SPD_PART_NUMBER + 17, Some(0xFF))]);
        assert_eq!(decode_part_number(&values), "F4-3600C14-16GVKA");
    }

    #[test]
    fn a_non_printable_byte_disqualifies_the_block() {
        // This is the case that matters: the lower half of the SPD holds
        // timing parameters, which are not ASCII, so a window on the wrong
        // half must not decode as though it were the right one.
        let values = bench_values(&[(SPD_PART_NUMBER + 2, Some(0x0C))]);
        assert_eq!(decode_part_number(&values), "");
    }

    #[test]
    fn an_unreadable_byte_is_skipped_rather_than_faked() {
        let values = bench_values(&[(SPD_PART_NUMBER, None)]);
        assert_eq!(decode_part_number(&values), "4-3600C14-16GVKA");
    }

    // --- SerialNumberTest

    #[test]
    fn an_all_zero_serial_matches_firmware_and_reference() {
        assert_eq!(decode_serial_number(&bench_values(&[])), "0");
    }

    #[test]
    fn an_all_f_block_is_unprogrammed_too() {
        let values = bench_values(&(0..4).map(|i| (SPD_SERIAL_NUMBER + i, Some(0xFF))).collect::<Vec<_>>());
        assert_eq!(decode_serial_number(&values), EM_DASH);
    }

    #[test]
    fn a_programmed_serial_reads_as_its_bytes() {
        let values = bench_values(&[
            (SPD_SERIAL_NUMBER, Some(0x00)),
            (SPD_SERIAL_NUMBER + 1, Some(0x00)),
            (SPD_SERIAL_NUMBER + 2, Some(0xAB)),
            (SPD_SERIAL_NUMBER + 3, Some(0xCD)),
        ]);
        assert_eq!(decode_serial_number(&values), "0000ABCD");
    }

    #[test]
    fn an_unreadable_byte_reports_nothing() {
        let values = bench_values(&[(SPD_SERIAL_NUMBER + 1, None)]);
        assert_eq!(decode_serial_number(&values), EM_DASH);
    }

    // --- DecodeIdentityTest

    #[test]
    fn the_bench_block_decodes_to_what_the_kit_is() {
        let found = decode_identity(&bench_values(&[])).unwrap();
        assert_eq!(found.part_number, "F4-3600C14-16GVKA");
        assert_eq!(found.module_manufacturer, "G.Skill");
        assert_eq!(found.dram_manufacturer, "Samsung");
        // Firmware and the reference expose this all-zero block as serial 0.
        assert_eq!(found.serial_number, "0");
        assert_eq!(found.manufacture_date, EM_DASH);
    }

    #[test]
    fn a_programmed_date_decodes() {
        let values = bench_values(&[(SPD_MFG_YEAR, Some(0x23)), (SPD_MFG_WEEK, Some(0x31))]);
        assert_eq!(decode_identity(&values).unwrap().manufacture_date, "31 / 2023");
    }

    #[test]
    fn a_block_that_fails_the_alignment_check_decodes_to_nothing() {
        // The lower half in the window: the device type byte lands where the
        // part number would start, and nothing here is ASCII.
        let values: SpdBytes = (0..SPD_PART_NUMBER_LENGTH).map(|i| (SPD_PART_NUMBER + i, 0x0C)).collect();
        assert_eq!(decode_identity(&values), None);
    }

    #[test]
    fn a_short_run_of_printable_bytes_is_not_a_part_number() {
        let values = SpdBytes::from([(SPD_PART_NUMBER, 0x41), (SPD_PART_NUMBER + 1, 0x42)]);
        assert_eq!(decode_identity(&values), None);
    }

    #[test]
    fn the_lower_half_is_recognisable_by_its_device_type() {
        // Not used to decide anything -- the part number does that -- but the
        // constant is what makes the negative case checkable by hand.
        assert_eq!(SPD_DEVICE_TYPE, 0x02);
        assert_eq!(SPD_DEVICE_TYPE_DDR4, 0x0C);
    }

    /// Answers one address, from the bench block, and records what was asked.
    struct FakeReader {
        answering: Vec<u8>,
        values: SpdBytes,
        driver_open: bool,
        reads: Mutex<Vec<(u8, u32)>>,
        writes: Mutex<Vec<String>>,
        page_reads: Mutex<Vec<(u8, i64, i64, u8)>>,
    }

    impl FakeReader {
        fn new(values: Option<SpdBytes>, driver_open: bool) -> Arc<Self> {
            Arc::new(FakeReader {
                answering: vec![0x51],
                values: values.unwrap_or_else(|| bench_values(&[])),
                driver_open,
                reads: Mutex::new(Vec::new()),
                writes: Mutex::new(Vec::new()),
                page_reads: Mutex::new(Vec::new()),
            })
        }
    }

    impl SmbusReader for FakeReader {
        fn is_driver_open(&self) -> bool {
            self.driver_open
        }

        fn probe_address(&self, address: u8, _controller: u8) -> bool {
            self.answering.contains(&address)
        }

        fn read_byte(&self, address: u8, register: u8, _controller: u8) -> Result<u8, SmbusError> {
            self.reads.lock().unwrap().push((address, register as u32));
            self.values.get(&(register as u32)).copied().ok_or_else(|| SmbusError::Bus("no byte".into()))
        }

        fn read_ddr4_spd(&self, address: u8, offset: i64, length: i64, controller: u8) -> Result<SpdBytes, SmbusError> {
            self.page_reads.lock().unwrap().push((address, offset, length, controller));
            let mut values = SpdBytes::new();
            for position in offset..offset + length {
                let register = (position - 0x100) as u32;
                self.reads.lock().unwrap().push((address, register));
                if let Some(value) = self.values.get(&register) {
                    values.insert(position as u32, *value);
                }
            }
            Ok(values)
        }

        fn write_byte(&self, _address: u8, _register: u8, _value: u8, _controller: u8) -> Result<(), SmbusError> {
            self.writes.lock().unwrap().push("write_byte".into());
            panic!("the DDR4 SPD reader must never write");
        }
    }

    fn factory(reader: &Arc<FakeReader>) -> impl Fn() -> Result<Arc<dyn SmbusReader>, SmbusError> + use<> {
        let reader = reader.clone();
        move || Ok(reader.clone() as Arc<dyn SmbusReader>)
    }

    // --- ReadIdentityTest

    #[test]
    fn an_answering_module_is_decoded() {
        let reader = FakeReader::new(None, true);
        let found = read_identity(Some(&factory(&reader)), false);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].part_number, "F4-3600C14-16GVKA");
        assert_eq!(found[0].address, 0x51);
    }

    #[test]
    fn nothing_is_written_to_the_bus() {
        // The whole reason this module exists rather than reusing the DDR5
        // one: selecting a page is a write, and on DDR4 that write lands on
        // the SPD array itself.
        let reader = FakeReader::new(None, true);
        read_identity(Some(&factory(&reader)), false);
        assert!(reader.writes.lock().unwrap().is_empty());
    }

    #[test]
    fn the_upper_ddr4_page_is_read() {
        let reader = FakeReader::new(None, true);
        read_identity(Some(&factory(&reader)), false);
        assert_eq!(*reader.page_reads.lock().unwrap(), vec![(0x51, 0x140, 0x21, 0x00)]);
    }

    #[test]
    fn no_driver_means_no_identity_rather_than_an_error() {
        let reader = FakeReader::new(None, false);
        assert_eq!(read_identity(Some(&factory(&reader)), false), vec![]);
    }

    #[test]
    fn a_reader_that_raises_reports_no_identity() {
        let broken = || -> Result<Arc<dyn SmbusReader>, SmbusError> { Err(SmbusError::Bus("no driver".into())) };
        assert_eq!(read_identity(Some(&broken), false), vec![]);
    }

    #[test]
    fn an_address_whose_block_does_not_align_is_dropped() {
        let reader = FakeReader::new(Some(SpdBytes::from([(SPD_PART_NUMBER, 0x0C)])), true);
        assert_eq!(read_identity(Some(&factory(&reader)), false), vec![]);
    }

    #[test]
    fn only_the_identity_registers_are_read() {
        let reader = FakeReader::new(None, true);
        read_identity(Some(&factory(&reader)), false);
        let asked: std::collections::BTreeSet<u32> = reader.reads.lock().unwrap().iter().map(|(_, r)| *r).collect();
        assert!(!asked.is_empty());
        assert!(asked.iter().all(|r| (SPD_MODULE_MFG_ID..0x61).contains(r)));
    }
}
