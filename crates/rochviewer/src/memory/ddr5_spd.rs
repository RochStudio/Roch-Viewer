//! DDR5 SPD identity: what the module itself reports about its DRAM.
//!
//! SMBIOS tells us the module SKU, its capacity and its rank count, but not
//! which DRAM sits on it. The SPD EEPROM does: a JEP106 manufacturer ID and a
//! die stepping, written by the module maker rather than inferred from the
//! part number. This module reads those bytes over the existing read-only SPD
//! path.
//!
//! Offsets are the JESD400-5 identity block, cross-checked against
//! ZenStates-Core (`DRAM/DDR5/Spd/Ddr5SpdDecoder.cs`) and confirmed on the
//! bench: reading the part number at 0x209 returns the SKU printed on the
//! label, which pins the whole block's alignment, and the manufacturer at
//! 0x228 then decodes to the DRAM maker the same DIMM reports in ZenTimings.

use super::ddr_generation::{DdrGeneration, detect_ddr_generation};
use super::ddr5_telemetry::{ReaderFactory, SmbusError, SpdBytes, default_smbus_backend};
use super::spd_profiles::{SpdFields, SpdValue};
use crate::memo::Memo;

// JESD400-5 identity block (module-specific section, page 4).
pub const SPD_MODULE_MFG_ID: u32 = 0x200; // 2 bytes, JEP106
pub const SPD_MFG_YEAR: u32 = 0x203; // BCD, years since 2000
pub const SPD_MFG_WEEK: u32 = 0x204; // BCD, ISO week
pub const SPD_SERIAL_NUMBER: u32 = 0x205; // 4 bytes, vendor-assigned
pub const SPD_SERIAL_LENGTH: u32 = 4;
pub const SPD_PART_NUMBER: u32 = 0x209; // 30 ASCII bytes
pub const SPD_PART_NUMBER_LENGTH: u32 = 30;
pub const SPD_DRAM_MFG_ID: u32 = 0x228; // 2 bytes, JEP106
pub const SPD_DRAM_STEPPING: u32 = 0x22A; // die revision, vendor-defined
pub const SPD_IDENTITY_BLOCK: (u32, u32) = (SPD_MODULE_MFG_ID, 0x30);

pub const EM_DASH: &str = "—";

/// JEP106 (bank, code) -> name, limited to DRAM makers, taken verbatim from
/// ZenStates-Core's ManufacturerBanks. Anything absent is reported as its raw
/// ID rather than guessed at.
pub const JEP106_VENDORS: &[((u32, u8), &str)] = &[
    ((1, 0x2C), "Micron Technology"),
    ((1, 0xAD), "SK hynix"),
    ((1, 0xCE), "Samsung"),
    ((1, 0xDA), "Winbond Electronic"),
    ((3, 0xFE), "Elpida"),
    ((4, 0x0B), "Nanya Technology"),
    ((5, 0xC8), "Powerchip Semiconductor"),
    ((9, 0xD5), "Etron Technology Inc"),
    ((11, 0x91), "CXMT"),
    // Module makers, for the module-vendor field rather than the DRAM one.
    // Firmware often leaves SMBIOS Manufacturer as "Unknown" while the SPD
    // carries this: 0x04CD on the bench kit, whose part number is a G.Skill
    // SKU, so the two agree.
    ((5, 0xCD), "G.Skill"),
    // Bank 7 code 0x6D, read as 0x866D on the LGA1700 kit. SMBIOS calls the
    // same module "V-Color Technology Inc"; this is the name the module itself
    // carries, and what CPU-Z shows against it.
    ((7, 0x6D), "V-Color Technology"),
    // The devices on a DDR5 module besides its DRAM: its SPD hub and its PMIC.
    // Named from the JEP106 list (OpenOCD's jep106.inc carries it): bank 1
    // code 0x33 is IDT and bank 11 code 0x0C Richtek Power, read here as
    // 0x80B3 and 0x8A8C on the bench kit's hub and PMIC. IDT is Renesas now,
    // and it is Renesas that sells the SPD5118 hubs under that ID.
    ((1, 0xB3), "Renesas (IDT)"),
    ((11, 0x8C), "Richtek"),
    // Bank 7 code 0x32, "Montage Technology Group" in the same list: the
    // SPD5118 hub on the X870 bench's G.Skill kit reads 0x8632, and
    // a reference tool names that hub Montage too.
    ((7, 0x32), "Montage"),
];

// Which SPD hubs answered last, so repeat reads skip the bus scan.
static CACHE: Memo<Vec<SpdIdentity>> = Memo::new();

// This block is only reachable on DDR5, and asking for it on DDR4 is worse
// than useless.
//
// The identity lives at 0x200, page 4 of the hub's EEPROM window, and the page
// has to be selected before the read. A DDR4 module has no hub and no page
// register: the same transaction reaches a plain I2C EEPROM, where the command
// byte the selector uses is not a page selector at all but an offset into the
// SPD array. So the read does not merely come back wrong -- which it did, a
// part number of ">>> '" and a die of "0xA0" on the Z790 DDR4 bench -- it aims
// a write at a module's own SPD. The PCH's SPD Write Disable stopped it
// reaching the module here, but that is a board default, not a guarantee.
//
// DDR4 identity is not reachable through this transport in any case: its bytes
// sit at 0x140-0x160 in page 1, and DDR4 selects pages by writing to the
// separate SPA0/SPA1 addresses, which this transport does not allow and which
// SPD Write Disable blocks. Callers fall back to SMBIOS, which carries the
// part number, the module maker and enough to name the DRAM.
pub const DDR4_GENERATION: DdrGeneration = DdrGeneration::Ddr4;

/// One module's identity, as the SPD reports it. Shared by the DDR5 and DDR4
/// readers; the DDR4 block has no die name (`dram_die` is `None`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpdIdentity {
    pub part_number: String,
    pub module_manufacturer: String,
    pub serial_number: String,
    pub manufacture_date: String,
    pub dram_manufacturer: String,
    pub dram_stepping: Option<u8>,
    pub dram_die: Option<String>,
    pub address: u8,
    pub controller: u8,
}

impl SpdIdentity {
    /// A field by its Python dict key, as text (`entry.get(field)`).
    pub fn get(&self, field: &str) -> Option<String> {
        match field {
            "part_number" => Some(self.part_number.clone()),
            "module_manufacturer" => Some(self.module_manufacturer.clone()),
            "serial_number" => Some(self.serial_number.clone()),
            "manufacture_date" => Some(self.manufacture_date.clone()),
            "dram_manufacturer" => Some(self.dram_manufacturer.clone()),
            "dram_stepping" => self.dram_stepping.map(|s| s.to_string()),
            "dram_die" => self.dram_die.clone(),
            "address" => Some(self.address.to_string()),
            "controller" => Some(self.controller.to_string()),
            _ => None,
        }
    }

    /// The identity as SPD record fields, for merging into an SPD tab record.
    pub fn fields(&self) -> SpdFields {
        let mut fields = SpdFields::new();
        fields.insert("part_number".into(), SpdValue::text(&self.part_number));
        fields.insert("module_manufacturer".into(), SpdValue::text(&self.module_manufacturer));
        fields.insert("serial_number".into(), SpdValue::text(&self.serial_number));
        fields.insert("manufacture_date".into(), SpdValue::text(&self.manufacture_date));
        fields.insert("dram_manufacturer".into(), SpdValue::text(&self.dram_manufacturer));
        fields.insert("dram_stepping".into(), self.dram_stepping.map_or(SpdValue::None, |s| SpdValue::Int(s as i64)));
        if let Some(die) = &self.dram_die {
            fields.insert("dram_die".into(), SpdValue::text(die));
        }
        fields.insert("address".into(), SpdValue::Int(self.address as i64));
        fields.insert("controller".into(), SpdValue::Int(self.controller as i64));
        fields
    }
}

/// "DDR4", "DDR5", or `None` when nothing here can say.
pub fn installed_generation() -> Option<DdrGeneration> {
    Some(detect_ddr_generation())
}

/// The vendor name for a JEP106 pair, or its raw ID.
///
/// The low byte carries the continuation count in bits 6:0 (bit 7 is odd
/// parity), so the bank is that count plus one; the high byte is the code.
pub fn decode_jep106_id(lsb: Option<u8>, msb: Option<u8>) -> String {
    let (Some(lsb), Some(msb)) = (lsb, msb) else {
        return EM_DASH.to_string();
    };
    let bank = (lsb as u32 & 0x7F) + 1;
    let code = msb;
    if let Some((_, name)) = JEP106_VENDORS.iter().find(|(key, _)| *key == (bank, code)) {
        return name.to_string();
    }
    format!("0x{lsb:02X}{code:02X}")
}

/// Name the die from the stepping byte, or report the byte itself.
///
/// Only SK hynix is decoded, and only for the 0x4n family: 0x41 is A-die,
/// 0x42 B-die, and so on. That is the rule ZenTimings applies, and it agrees
/// on this bench with the part-number table this project already carried
/// (0x41 on a kit independently known to be A-die). No other vendor's
/// stepping encoding is claimed here -- for those the raw byte is shown,
/// which is honest about what the module actually reported.
pub fn decode_die(manufacturer: Option<&str>, stepping: Option<u8>) -> String {
    let Some(stepping) = stepping else {
        return EM_DASH.to_string();
    };
    if manufacturer.unwrap_or("").to_lowercase().contains("hynix") {
        let (family, index) = (stepping >> 4, stepping & 0x0F);
        if family == 0x4 && (1..=15).contains(&index) {
            return format!("{}-die", (b'A' + index - 1) as char);
        }
    }
    format!("0x{stepping:02X}")
}

/// A BCD byte as an int, or `None` if either nibble is not a digit.
pub fn decode_bcd(value: Option<u8>) -> Option<u32> {
    let value = value?;
    let (high, low) = (value >> 4, value & 0x0F);
    if high > 9 || low > 9 {
        return None;
    }
    Some(high as u32 * 10 + low as u32)
}

/// "WW / YYYY" for the module's build date, or an em dash.
///
/// Both bytes are BCD, the year counted from 2000 -- the same pair CPU-Z
/// shows as Week/Year on its SPD tab. Confirmed against the bench kit, which
/// reads 0x25/0x04 where CPU-Z reports 04 / 25.
///
/// An unprogrammed module leaves these at 0x00 or 0xFF, so a week outside
/// 1-53 is reported as nothing rather than as week 0 of 2000.
pub fn decode_manufacture_date(year_byte: Option<u8>, week_byte: Option<u8>) -> String {
    match (decode_bcd(year_byte), decode_bcd(week_byte)) {
        (Some(year), Some(week)) if (1..=53).contains(&week) => format!("{:02} / {}", week, 2000 + year),
        _ => EM_DASH.to_string(),
    }
}

/// The module serial as eight hex digits, or an em dash.
///
/// Four raw bytes, printed as digits rather than converted to a decimal
/// number: that is how the vendor prints it on the label and how CPU-Z shows
/// it, and the two modules here read ...4996 and ...4997, consecutive.
pub fn decode_serial_number(values: &SpdBytes) -> String {
    let mut digits = String::new();
    for offset in SPD_SERIAL_NUMBER..SPD_SERIAL_NUMBER + SPD_SERIAL_LENGTH {
        let Some(byte) = values.get(&offset) else {
            return EM_DASH.to_string();
        };
        digits.push_str(&format!("{byte:02X}"));
    }
    digits
}

/// Decode one SPD identity block, `{offset: byte}` as `read_spd` returns it.
pub fn decode_identity(values: &SpdBytes) -> SpdIdentity {
    let byte = |offset: u32| values.get(&offset).copied();

    let part_number: String = (SPD_PART_NUMBER..SPD_PART_NUMBER + SPD_PART_NUMBER_LENGTH)
        .filter_map(|offset| values.get(&offset).copied())
        .filter(|b| (0x20..0x7F).contains(b))
        .map(|b| b as char)
        .collect::<String>()
        .trim()
        .to_string();

    let manufacturer = decode_jep106_id(byte(SPD_DRAM_MFG_ID), byte(SPD_DRAM_MFG_ID + 1));
    let stepping = byte(SPD_DRAM_STEPPING);
    SpdIdentity {
        part_number,
        module_manufacturer: decode_jep106_id(byte(SPD_MODULE_MFG_ID), byte(SPD_MODULE_MFG_ID + 1)),
        serial_number: decode_serial_number(values),
        manufacture_date: decode_manufacture_date(byte(SPD_MFG_YEAR), byte(SPD_MFG_WEEK)),
        dram_die: Some(decode_die(Some(&manufacturer), stepping)),
        dram_manufacturer: manufacturer,
        dram_stepping: stepping,
        address: 0,
        controller: 0,
    }
}

/// One identity per SPD hub that answers, or none.
///
/// Cached: the System Info rows read it during startup and the bus scan is
/// the slow part. An empty list means no hub answered -- no driver, no
/// permissions, or a platform whose SMBus path this project has no transport
/// for -- and callers are expected to fall back to what SMBIOS told them.
///
/// DDR4 gets the same empty list without touching the bus at all. See
/// DDR4_GENERATION for why running this there both misreads the module and
/// points a write at its SPD.
///
/// The transport is the platform's; see [`default_smbus_backend`].
pub fn read_identity(
    reader_factory: Option<ReaderFactory>,
    refresh: bool,
    generation: Option<DdrGeneration>,
) -> Vec<SpdIdentity> {
    let generation = generation.or_else(installed_generation);
    if generation == Some(DDR4_GENERATION) {
        return Vec::new();
    }

    // An injected factory is a test or a probe: never served from, nor written
    // to, the cache the app shares.
    let use_cache = reader_factory.is_none();
    if use_cache
        && !refresh
        && let Some(cached) = CACHE.get()
    {
        return cached;
    }

    let backend = default_smbus_backend();
    if backend.is_none() && reader_factory.is_none() {
        return Vec::new();
    }
    let run = || -> Result<Vec<SpdIdentity>, SmbusError> {
        let (default_factory, controller_offsets, hub_addresses): (Option<ReaderFactory>, &[u8], &[u8]) = match &backend
        {
            // A caller that supplied a reader has supplied the transport;
            // all that is missing is where to point it, and that is
            // JEDEC's, not the platform's -- SPD hubs answer at 0x50-0x57
            // behind any host controller. Without this an injected reader
            // was discarded on a machine with no recognised backend,
            // which is exactly where a test runs and exactly where the
            // injection exists for.
            None => {
                use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES};
                (None, &CONTROLLER_OFFSETS, &SPD_HUB_ADDRESSES)
            }
            Some(found) => (Some(&found.factory), found.controllers, found.hubs),
        };
        let Some(reader_factory) = reader_factory.or(default_factory) else {
            return Ok(Vec::new());
        };

        let mut modules = Vec::new();
        let reader = reader_factory()?;
        if reader.is_driver_open() {
            let (offset, length) = SPD_IDENTITY_BLOCK;
            for &controller in controller_offsets {
                for &address in hub_addresses {
                    if !reader.probe_address(address, controller) {
                        continue;
                    }
                    let values = match reader.read_spd(address, offset as i64, length as i64, controller) {
                        Ok(values) => values,
                        Err(error) if error.is_transfer() => continue,
                        Err(error) => return Err(error),
                    };
                    let mut identity = decode_identity(&values);
                    identity.address = address;
                    identity.controller = controller;
                    modules.push(identity);
                }
                // DIMMs live on one controller; once it answered, stop probing
                // the other, since probing an empty address blocks.
                if !modules.is_empty() {
                    break;
                }
            }
        }
        Ok(modules)
    };
    let modules = run().unwrap_or_default();

    if use_cache {
        CACHE.set(modules.clone());
    }
    modules
}

// The whole EEPROM: the base block, the identity at 0x200 and the XMP / EXPO
// extension area at 0x280-0x3FF. read_spd pages through it under the bus
// lock and puts the hub back on the page it found it on.
pub const SPD_FULL_LENGTH: u32 = 0x400;

/// All of one module's SPD as `{offset: byte}`, or empty if unreadable.
///
/// Not cached: the SPD tab reads it once, off the UI thread, when the page is
/// first opened. DDR4 never reaches here -- see DDR4_GENERATION.
pub fn read_spd_bytes(address: Option<u8>, controller: Option<u8>, reader_factory: Option<ReaderFactory>) -> SpdBytes {
    let (Some(address), Some(controller)) = (address, controller) else {
        return SpdBytes::new();
    };
    let backend;
    let reader_factory: ReaderFactory = match reader_factory {
        Some(factory) => factory,
        None => {
            backend = match default_smbus_backend() {
                Some(found) => found,
                None => return SpdBytes::new(),
            };
            &backend.factory
        }
    };
    let Ok(reader) = reader_factory() else {
        return SpdBytes::new();
    };
    if !reader.is_driver_open() {
        return SpdBytes::new();
    }
    reader.read_spd(address, 0, SPD_FULL_LENGTH as i64, controller).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ddr5_telemetry::SmbusReader;
    use std::sync::{Arc, Mutex};

    // Captured from the bench kit (G.Skill F5-6000J2636G16G, SPD hub 0x51):
    // module vendor 04 CD, serial, part number at 0x209 padded with spaces,
    // revision 00, DRAM vendor 80 AD, stepping 0x41.
    fn bench_block() -> Vec<u8> {
        let mut block = vec![0x04, 0xCD, 0x00, 0x25, 0x07, 0xAA, 0x5A, 0xE5, 0xFA];
        let mut part = b"F5-6000J2636G16G".to_vec();
        part.resize(30, b' ');
        block.extend(part);
        block.extend([0x00, 0x80, 0xAD, 0x41, 0x00, 0x00, 0x00, 0x00, 0x00]);
        block
    }

    fn bench_values(overrides: &[(u32, u8)]) -> SpdBytes {
        let mut values: SpdBytes =
            bench_block().into_iter().enumerate().map(|(i, b)| (SPD_MODULE_MFG_ID + i as u32, b)).collect();
        values.extend(overrides.iter().copied());
        values
    }

    // --- Jep106Test

    #[test]
    fn bank_comes_from_the_continuation_count() {
        // 0x80 = parity bit set, continuation count 0 -> bank 1, code 0xAD.
        assert_eq!(decode_jep106_id(Some(0x80), Some(0xAD)), "SK hynix");
        assert_eq!(decode_jep106_id(Some(0x00), Some(0xAD)), "SK hynix");
        assert_eq!(decode_jep106_id(Some(0x80), Some(0x2C)), "Micron Technology");
        assert_eq!(decode_jep106_id(Some(0x80), Some(0xCE)), "Samsung");
        // Bank 4 (continuation count 3), Nanya.
        assert_eq!(decode_jep106_id(Some(0x03), Some(0x0B)), "Nanya Technology");
    }

    #[test]
    fn the_montage_spd_hub_is_named() {
        // 0x86: parity set, continuation count 6 -> bank 7, code 0x32. Read
        // off the X870 bench kit's SPD5118 hub.
        assert_eq!(decode_jep106_id(Some(0x86), Some(0x32)), "Montage");
    }

    #[test]
    fn an_unlisted_vendor_reports_its_raw_id_rather_than_a_guess() {
        assert_eq!(decode_jep106_id(Some(0x80), Some(0x11)), "0x8011");
    }

    #[test]
    fn missing_bytes_report_nothing() {
        assert_eq!(decode_jep106_id(None, Some(0xAD)), EM_DASH);
        assert_eq!(decode_jep106_id(Some(0x80), None), EM_DASH);
    }

    // --- DieDecodeTest

    #[test]
    fn hynix_4n_stepping_names_the_die() {
        assert_eq!(decode_die(Some("SK hynix"), Some(0x41)), "A-die");
        assert_eq!(decode_die(Some("SK hynix"), Some(0x42)), "B-die");
        assert_eq!(decode_die(Some("SK hynix"), Some(0x4D)), "M-die");
    }

    #[test]
    fn other_hynix_families_are_not_named() {
        // Outside the 0x4n family the letter rule is not claimed to hold.
        assert_eq!(decode_die(Some("SK hynix"), Some(0x10)), "0x10");
        assert_eq!(decode_die(Some("SK hynix"), Some(0x40)), "0x40");
    }

    #[test]
    fn other_vendors_report_the_raw_stepping() {
        assert_eq!(decode_die(Some("Samsung"), Some(0x41)), "0x41");
        assert_eq!(decode_die(Some("Micron Technology"), Some(0x02)), "0x02");
    }

    #[test]
    fn missing_stepping_reports_nothing() {
        assert_eq!(decode_die(Some("SK hynix"), None), EM_DASH);
    }

    // --- SerialNumberTest

    fn serial(bytes: &[u8]) -> SpdBytes {
        bytes.iter().enumerate().map(|(i, b)| (SPD_SERIAL_NUMBER + i as u32, *b)).collect()
    }

    #[test]
    fn the_serial_is_printed_as_digits_not_converted() {
        // CPU-Z shows 00004996 for the first module on the DDR5 bench and
        // 00004997 for the second. Read as a number those would be 18838 and
        // 18839, which is not what is printed on either label.
        assert_eq!(decode_serial_number(&serial(&[0x00, 0x00, 0x49, 0x96])), "00004996");
    }

    #[test]
    fn every_byte_is_kept_including_leading_zeroes() {
        assert_eq!(decode_serial_number(&serial(&[0x00, 0x0A, 0xB0, 0x0F])), "000AB00F");
    }

    #[test]
    fn a_short_read_reports_nothing() {
        assert_eq!(decode_serial_number(&serial(&[0x00])), EM_DASH);
    }

    // --- VendorNameTest

    #[test]
    fn the_lga1700_module_vendor_decodes_to_its_name() {
        // 0x866D off the bench kit: continuation count 6 -> bank 7, code 0x6D.
        // SMBIOS calls the same module "V-Color Technology Inc"; this is the
        // name the module carries, and what CPU-Z shows.
        assert_eq!(decode_jep106_id(Some(0x86), Some(0x6D)), "V-Color Technology");
    }

    // --- ManufactureDateTest

    #[test]
    fn the_lga1700_kit_decodes_to_what_cpuz_reports() {
        // V-Color TMXFL1680838KWK, read from both hubs on the DDR5 bench:
        // 0x203 = 0x25, 0x204 = 0x04, where CPU-Z shows Week/Year 04 / 25.
        assert_eq!(decode_manufacture_date(Some(0x25), Some(0x04)), "04 / 2025");
    }

    #[test]
    fn both_bytes_are_bcd_rather_than_binary() {
        // 0x24 is week 24, not 36; a binary read would say the latter.
        assert_eq!(decode_manufacture_date(Some(0x25), Some(0x24)), "24 / 2025");
        assert_eq!(decode_manufacture_date(Some(0x23), Some(0x53)), "53 / 2023");
    }

    #[test]
    fn an_unprogrammed_module_reports_nothing() {
        assert_eq!(decode_manufacture_date(Some(0x00), Some(0x00)), EM_DASH);
        assert_eq!(decode_manufacture_date(Some(0xFF), Some(0xFF)), EM_DASH);
        assert_eq!(decode_manufacture_date(None, None), EM_DASH);
    }

    #[test]
    fn a_week_past_the_calendar_is_not_reported_as_a_date() {
        assert_eq!(decode_manufacture_date(Some(0x25), Some(0x54)), EM_DASH);
        // Nibbles that are not digits are not a BCD week either.
        assert_eq!(decode_manufacture_date(Some(0x25), Some(0x1A)), EM_DASH);
    }

    // --- DecodeIdentityTest

    #[test]
    fn the_bench_block_decodes_to_the_kit_on_the_bench() {
        let identity = decode_identity(&bench_values(&[]));
        assert_eq!(identity.part_number, "F5-6000J2636G16G");
        assert_eq!(identity.dram_manufacturer, "SK hynix");
        assert_eq!(identity.dram_stepping, Some(0x41));
        assert_eq!(identity.dram_die.as_deref(), Some("A-die"));
        assert_eq!(identity.manufacture_date, "07 / 2025");
        assert_eq!(identity.serial_number, "AA5AE5FA");
    }

    #[test]
    fn the_date_comes_from_the_block_rather_than_a_constant() {
        let identity = decode_identity(&bench_values(&[(SPD_MFG_YEAR, 0x22), (SPD_MFG_WEEK, 0x51)]));
        assert_eq!(identity.manufacture_date, "51 / 2022");
    }

    #[test]
    fn a_samsung_module_reports_its_stepping_byte() {
        let identity = decode_identity(&bench_values(&[(SPD_DRAM_MFG_ID + 1, 0xCE), (SPD_DRAM_STEPPING, 0x11)]));
        assert_eq!(identity.dram_manufacturer, "Samsung");
        assert_eq!(identity.dram_die.as_deref(), Some("0x11"));
    }

    #[test]
    fn a_short_read_does_not_raise() {
        let identity = decode_identity(&SpdBytes::from([(SPD_PART_NUMBER, b'F')]));
        assert_eq!(identity.part_number, "F");
        assert_eq!(identity.dram_manufacturer, EM_DASH);
        assert_eq!(identity.dram_die.as_deref(), Some(EM_DASH));
        assert_eq!(identity.manufacture_date, EM_DASH);
    }

    struct FakeReader {
        hubs: Vec<(u8, u8)>,
        driver_open: bool,
        probed: Mutex<Vec<(u8, u8)>>,
    }

    impl FakeReader {
        fn new(hubs: &[(u8, u8)], driver_open: bool) -> Arc<Self> {
            Arc::new(FakeReader { hubs: hubs.to_vec(), driver_open, probed: Mutex::new(Vec::new()) })
        }
    }

    impl SmbusReader for FakeReader {
        fn is_driver_open(&self) -> bool {
            self.driver_open
        }

        fn probe_address(&self, address: u8, controller: u8) -> bool {
            self.probed.lock().unwrap().push((controller, address));
            self.hubs.contains(&(controller, address))
        }

        fn read_spd(&self, _address: u8, _offset: i64, _length: i64, _controller: u8) -> Result<SpdBytes, SmbusError> {
            Ok(bench_values(&[]))
        }
    }

    fn factory(reader: &Arc<FakeReader>) -> impl Fn() -> Result<Arc<dyn SmbusReader>, SmbusError> + use<> {
        let reader = reader.clone();
        move || Ok(reader.clone() as Arc<dyn SmbusReader>)
    }

    // --- ReadIdentityTest: the DDR5 path. Generation is stated rather than
    // sniffed from the bench.
    //
    // These describe what an SPD5 hub answers, so they say DDR5 outright: run
    // on a DDR4 machine the reader refuses before it reaches the bus, which
    // is the behaviour ddr4_is_refused_without_touching_the_bus covers.

    #[test]
    fn every_answering_hub_is_decoded() {
        let reader = FakeReader::new(&[(0x00, 0x51), (0x00, 0x53)], true);
        let found = read_identity(Some(&factory(&reader)), false, Some(DdrGeneration::Ddr5));
        assert_eq!(found.len(), 2);
        assert_eq!(found.iter().map(|e| e.address).collect::<Vec<_>>(), vec![0x51, 0x53]);
        assert_eq!(found[0].dram_die.as_deref(), Some("A-die"));
    }

    #[test]
    fn the_second_controller_is_not_probed_once_dimms_answered() {
        let reader = FakeReader::new(&[(0x00, 0x51)], true);
        read_identity(Some(&factory(&reader)), false, Some(DdrGeneration::Ddr5));
        assert!(reader.probed.lock().unwrap().iter().all(|(c, _)| *c == 0x00));
    }

    #[test]
    fn ddr4_is_refused_without_touching_the_bus() {
        // The page selector this path uses is a write, and on DDR4 the same
        // transaction lands on the SPD array rather than on a hub's page
        // register. It must not reach the bus at all.
        let reader = FakeReader::new(&[(0x00, 0x51)], true);
        assert_eq!(read_identity(Some(&factory(&reader)), false, Some(DdrGeneration::Ddr4)), vec![]);
        assert!(reader.probed.lock().unwrap().is_empty());
    }

    #[test]
    fn no_driver_means_no_identity_rather_than_an_error() {
        let reader = FakeReader::new(&[(0x00, 0x51)], false);
        assert_eq!(read_identity(Some(&factory(&reader)), false, None), vec![]);
    }

    #[test]
    fn a_reader_that_raises_reports_no_identity() {
        let broken = || -> Result<Arc<dyn SmbusReader>, SmbusError> { Err(SmbusError::Bus("no driver".into())) };
        assert_eq!(read_identity(Some(&broken), false, None), vec![]);
    }
}
