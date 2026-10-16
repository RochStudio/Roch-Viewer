//! Read and decode the information shown on the SPD tab.
//!
//! The transport remains in the generation-specific SPD modules.  This module
//! turns their byte maps into a small display model and joins each SPD
//! address to the physical slot name reported by SMBIOS.
//!
//! The display model stays keyed by name, as the Python dicts were: a record
//! is a map of field name to [`SpdValue`] plus its profile columns, and each
//! profile column a map of row key to value. The SPD page asks for its rows by
//! key and prints whatever it finds, and the decoders, the identity readers
//! and the slot join each add a different set of fields.

use std::collections::BTreeMap;
use std::fmt;

use super::ddr5_spd::{EM_DASH, decode_jep106_id, decode_manufacture_date};
use super::ddr5_telemetry::{ReaderFactory, SpdBytes};
use super::dimm_inventory::DimmModule;
use crate::pyfmt;

/// One displayed value: what the Python dicts held.
#[derive(Clone, Debug, PartialEq)]
pub enum SpdValue {
    /// Python `None`: the page shows nothing.
    None,
    Int(i64),
    Text(String),
}

impl SpdValue {
    pub fn text(value: &str) -> Self {
        SpdValue::Text(value.to_string())
    }

    /// `str(value)`, or `None` for a missing value.
    pub fn display(&self) -> Option<String> {
        match self {
            SpdValue::None => None,
            SpdValue::Int(i) => Some(i.to_string()),
            SpdValue::Text(s) => Some(s.clone()),
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            SpdValue::Int(i) => Some(*i),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            SpdValue::Text(s) => Some(s),
            _ => None,
        }
    }

    /// Python truthiness: `None`, `0` and `""` are false.
    pub fn is_truthy(&self) -> bool {
        match self {
            SpdValue::None => false,
            SpdValue::Int(i) => *i != 0,
            SpdValue::Text(s) => !s.is_empty(),
        }
    }
}

impl fmt::Display for SpdValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SpdValue::None => f.write_str("None"),
            SpdValue::Int(i) => write!(f, "{i}"),
            SpdValue::Text(s) => f.write_str(s),
        }
    }
}

impl From<i64> for SpdValue {
    fn from(value: i64) -> Self {
        SpdValue::Int(value)
    }
}

impl From<&str> for SpdValue {
    fn from(value: &str) -> Self {
        SpdValue::Text(value.to_string())
    }
}

impl From<String> for SpdValue {
    fn from(value: String) -> Self {
        SpdValue::Text(value)
    }
}

impl From<Option<i64>> for SpdValue {
    fn from(value: Option<i64>) -> Self {
        value.map_or(SpdValue::None, SpdValue::Int)
    }
}

/// A record's named fields.
pub type SpdFields = BTreeMap<String, SpdValue>;

/// One profile column: row key -> value.
pub type SpdProfile = BTreeMap<&'static str, SpdValue>;

/// One module as the SPD tab shows it: its fields and its profile columns.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpdRecord {
    pub fields: SpdFields,
    pub profiles: Vec<SpdProfile>,
}

impl SpdRecord {
    pub fn get(&self, key: &str) -> Option<&SpdValue> {
        self.fields.get(key)
    }

    /// `str(record[key])`, `None` when absent or `None`.
    pub fn text(&self, key: &str) -> Option<String> {
        self.get(key).and_then(SpdValue::display)
    }

    pub fn set(&mut self, key: &str, value: impl Into<SpdValue>) {
        self.fields.insert(key.to_string(), value.into());
    }

    /// `record.update(fields)`.
    pub fn update(&mut self, fields: &SpdFields) {
        for (key, value) in fields {
            self.fields.insert(key.clone(), value.clone());
        }
    }

    fn truthy(&self, key: &str) -> bool {
        self.get(key).is_some_and(SpdValue::is_truthy)
    }
}

/// Decoding stopped where the Python raised. The message is the Python
/// exception's, which the SPD page printed.
pub type DecodeError = String;

/// The extension block's decode: its name and the profiles it carries.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XmpDecode {
    pub extension: String,
    pub profiles: Vec<SpdProfile>,
    /// EXPO's profiles (DDR5 only), kept apart from XMP's.
    pub expo_profiles: Vec<SpdProfile>,
}

pub const DDR4: u8 = 0x0C;
pub const MTB_PS: i64 = 125;
pub const FTB_PS: i64 = 1;

pub const DDR4_MODULE_TYPES: &[(u8, &str)] = &[
    (0x01, "RDIMM"),
    (0x02, "UDIMM"),
    (0x03, "SO-DIMM"),
    (0x04, "LRDIMM"),
    (0x05, "Mini-RDIMM"),
    (0x06, "Mini-UDIMM"),
    (0x08, "72b SO-RDIMM"),
    (0x09, "72b SO-UDIMM"),
    (0x0C, "16b SO-DIMM"),
    (0x0D, "32b SO-DIMM"),
];

fn table_get<K: PartialEq + Copy, V: Copy>(table: &[(K, V)], key: K) -> Option<V> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

fn byte(values: &SpdBytes, offset: u32) -> i64 {
    values.get(&offset).copied().unwrap_or(0) as i64
}

fn signed_byte(value: i64) -> i64 {
    let value = value & 0xFF;
    if value & 0x80 != 0 { value - 0x100 } else { value }
}

/// `upper` is an SPD offset, as in the Python (default 0).
fn time_ps(values: &SpdBytes, coarse_offset: u32, fine_offset: Option<u32>, upper: u32) -> i64 {
    let coarse = (byte(values, upper) << 8) | byte(values, coarse_offset);
    let fine = fine_offset.map_or(0, |offset| signed_byte(byte(values, offset)));
    coarse * MTB_PS + fine * FTB_PS
}

fn clock_cycles(time_ps: i64, tck_ps: i64) -> Option<i64> {
    if time_ps == 0 || tck_ps == 0 {
        return None;
    }
    // XMP defines a 0.01-clock guardband before rounding up.
    Some(((time_ps as f64 / tck_ps as f64) - 0.01).ceil() as i64)
}

fn data_rate(tck_ps: i64) -> Result<i64, DecodeError> {
    if tck_ps == 0 {
        return Err("float division by zero".into());
    }
    let raw = 2_000_000.0 / tck_ps as f64;
    let jedec_bins = [800, 1066, 1333, 1600, 1866, 2133, 2400, 2666, 2933, 3200];
    let mut nearest = jedec_bins[0];
    for value in jedec_bins {
        if (value as f64 - raw).abs() < (nearest as f64 - raw).abs() {
            nearest = value;
        }
    }
    Ok(if (nearest as f64 - raw).abs() <= 40.0 { nearest } else { pyfmt::round_int(raw / 100.0) * 100 })
}

fn supported_cls(values: &SpdBytes, start: u32) -> Vec<i64> {
    let mut result = Vec::new();
    for byte_index in 0..4 {
        let bits = byte(values, start + byte_index);
        for bit in 0..8 {
            if bits & (1 << bit) != 0 {
                result.push(7 + byte_index as i64 * 8 + bit);
            }
        }
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn profile(
    name: String,
    tck_ps: i64,
    taa_ps: i64,
    trcd_ps: i64,
    trp_ps: i64,
    tras_ps: i64,
    trc_ps: i64,
    voltage: f64,
) -> Result<Option<SpdProfile>, DecodeError> {
    if tck_ps == 0 {
        return Ok(None);
    }
    let clock_mhz = pyfmt::round_int(1_000_000.0 / tck_ps as f64);
    let data_rate = data_rate(tck_ps)?;
    Ok(Some(SpdProfile::from([
        ("name", SpdValue::Text(name)),
        ("frequency", SpdValue::Text(format!("{clock_mhz} MHz"))),
        ("data_rate", SpdValue::Text(format!("DDR4-{data_rate}"))),
        ("cl", clock_cycles(taa_ps, tck_ps).into()),
        ("trcd", clock_cycles(trcd_ps, tck_ps).into()),
        ("trp", clock_cycles(trp_ps, tck_ps).into()),
        ("tras", clock_cycles(tras_ps, tck_ps).into()),
        ("trc", clock_cycles(trc_ps, tck_ps).into()),
        ("voltage", SpdValue::Text(format!("{voltage:.2} V"))),
    ])))
}

/// Decode the module description and representative JEDEC profiles; `None`
/// when the bytes are not DDR4's.
pub fn decode_ddr4_base(values: &SpdBytes) -> Result<Option<SpdRecord>, DecodeError> {
    if byte(values, 2) != DDR4 as i64 {
        return Ok(None);
    }

    let density_code = byte(values, 4) & 0x0F;
    let device_width_code = byte(values, 12) & 0x07;
    let bus_width_code = byte(values, 13) & 0x07;
    let ranks = ((byte(values, 12) >> 3) & 0x07) + 1;
    let density_mbit = if density_code <= 7 { 256 << density_code } else { 0 };
    let device_width = if device_width_code <= 3 { 4 << device_width_code } else { 0 };
    let bus_width = if bus_width_code <= 3 { 8 << bus_width_code } else { 0 };
    let capacity_mb = if density_mbit != 0 && device_width != 0 && bus_width != 0 {
        (density_mbit * (bus_width / device_width) * ranks).div_euclid(8)
    } else {
        0
    };

    let tck_ps = time_ps(values, 18, Some(125), 0);
    let taa_ps = time_ps(values, 24, Some(123), 0);
    let trcd_ps = time_ps(values, 25, Some(122), 0);
    let trp_ps = time_ps(values, 26, Some(121), 0);
    let upper = byte(values, 27);
    let tras_ps = (((upper & 0x0F) << 8) | byte(values, 28)) * MTB_PS;
    let mut trc_ps = (((upper >> 4) << 8) | byte(values, 29)) * MTB_PS;
    trc_ps += signed_byte(byte(values, 120)) * FTB_PS;

    let mut profiles = Vec::new();
    let supported = supported_cls(values, 20);
    // The highest three useful CAS choices are what compact SPD viewers show.
    // Lower CL values down-bin the same timing set; keeping the last three
    // avoids a wide table without hiding the module's fastest JEDEC choice.
    for &cl in &supported[supported.len().saturating_sub(3)..] {
        let profile_tck = tck_ps.max((taa_ps as f64 / cl as f64).ceil() as i64);
        let item = profile(format!("JEDEC {cl}"), profile_tck, taa_ps, trcd_ps, trp_ps, tras_ps, trc_ps, 1.20)?;
        if let Some(mut item) = item {
            item.insert("cl", SpdValue::Int(cl));
            profiles.push(item);
        }
    }

    let module_code = (byte(values, 3) & 0x0F) as u8;
    let module_type = table_get(DDR4_MODULE_TYPES, module_code);
    let mut record = SpdRecord { fields: SpdFields::new(), profiles };
    record.set("memory_type", "DDR4");
    record.set("module_type", module_type.map(str::to_string).unwrap_or_else(|| format!("Type 0x{module_code:X}")));
    record.set(
        "capacity",
        if capacity_mb != 0 { format!("{} GB", capacity_mb.div_euclid(1024)) } else { EM_DASH.to_string() },
    );
    record.set("rank", format!("{ranks}R"));
    record.set(
        "max_bandwidth",
        if tck_ps != 0 {
            format!("DDR4-{} ({} MHz)", data_rate(tck_ps)?, pyfmt::round_int(1_000_000.0 / tck_ps as f64))
        } else {
            EM_DASH.to_string()
        },
    );
    Ok(Some(record))
}

/// Decode the two profile slots defined by Intel XMP 2.0.
pub fn decode_ddr4_xmp(values: &SpdBytes) -> Result<XmpDecode, DecodeError> {
    if (byte(values, 384), byte(values, 385)) != (0x0C, 0x4A) {
        return Ok(XmpDecode { extension: EM_DASH.to_string(), ..XmpDecode::default() });
    }
    let revision = byte(values, 387);
    let mut result =
        XmpDecode { extension: format!("XMP {}.{}", revision >> 4, revision & 0x0F), ..XmpDecode::default() };
    let enabled = byte(values, 386);
    for (index, base) in [393u32, 440].into_iter().enumerate() {
        if enabled & (1 << index) == 0 {
            continue;
        }
        let fine = if index == 0 { 431 } else { 478 };
        let tck_ps = time_ps(values, base + 3, Some(fine), 0);
        let taa_ps = time_ps(values, base + 8, Some(fine - 1), 0);
        let trcd_ps = time_ps(values, base + 9, Some(fine - 2), 0);
        let trp_ps = time_ps(values, base + 10, Some(fine - 3), 0);
        let upper = byte(values, base + 11);
        let tras_ps = (((upper & 0x0F) << 8) | byte(values, base + 12)) * MTB_PS;
        let mut trc_ps = (((upper >> 4) << 8) | byte(values, base + 13)) * MTB_PS;
        trc_ps += signed_byte(byte(values, fine - 4)) * FTB_PS;
        let voltage_byte = byte(values, base);
        let voltage = (if voltage_byte & 0x80 != 0 { 1.0 } else { 0.0 }) + (voltage_byte & 0x7F) as f64 / 100.0;
        let item =
            profile(format!("XMP-{}", data_rate(tck_ps)?), tck_ps, taa_ps, trcd_ps, trp_ps, tras_ps, trc_ps, voltage)?;
        if let Some(item) = item {
            result.profiles.push(item);
        }
    }
    Ok(result)
}

/// One complete DDR4 SPD display record, or `None` when the bytes are not
/// DDR4's. `identity` is merged over the decoded fields.
pub fn decode_ddr4_spd(values: &SpdBytes, identity: Option<&SpdFields>) -> Result<Option<SpdRecord>, DecodeError> {
    let Some(mut record) = decode_ddr4_base(values)? else {
        return Ok(None);
    };
    if let Some(identity) = identity {
        record.update(identity);
    }
    if !record.truthy("dram_die") {
        let manufacturer = record.text("dram_manufacturer");
        let stepping = record.get("dram_stepping").and_then(SpdValue::as_int).map(|s| (s & 0xFF) as u8);
        record.set("dram_die", super::ddr5_spd::decode_die(manufacturer.as_deref(), stepping));
    }
    let xmp = decode_ddr4_xmp(values)?;
    record.set("extension", xmp.extension);
    record.profiles.extend(xmp.profiles);
    Ok(Some(record))
}

// --- DDR5 (JESD400-5 base block, Intel XMP 3.0) ---------------------------
//
// DDR5 SPD states every timing in whole picoseconds, little-endian, rather
// than DDR4's medium/fine timebase pair. The offsets below were confirmed on
// an LGA1700 bench kit (V-Color TMXFL1680838KWK): the base block decodes to
// DDR5-4800 40-39-39-77, and XMP profile 1 to DDR5-8000 38-48-48-128 at
// 1.45 V, which is the kit's label and what the board trains it to.

pub const DDR5: u8 = 0x12;

pub const DDR5_MODULE_TYPES: &[(u8, &str)] = &[
    (0x01, "RDIMM"),
    (0x02, "UDIMM"),
    (0x03, "SO-DIMM"),
    (0x04, "LRDIMM"),
    (0x05, "CUDIMM"),
    (0x06, "CSODIMM"),
    (0x07, "MRDIMM"),
    (0x08, "CAMM2"),
];

/// JEDEC DDR5 data rates, each with the tCK the standard rounds it to.
pub const DDR5_SPEED_BINS: &[(i64, i64)] = &[
    (3200, 625),
    (3600, 555),
    (4000, 500),
    (4400, 454),
    (4800, 416),
    (5200, 384),
    (5600, 357),
    (6000, 333),
    (6400, 312),
    (6800, 294),
    (7200, 277),
    (7600, 263),
    (8000, 250),
    (8400, 238),
    (8800, 227),
];

pub const DDR5_TCK_MIN: u32 = 20;
pub const DDR5_CAS_MASK: u32 = 24; // five bytes, bit n = CL 20 + 2n
pub const DDR5_TAA: u32 = 30; // then tRCD, tRP, tRAS, tRC, two bytes each

// The module's nominal VDD, VDDQ and VPP, bits 7:4 of bytes 16, 17 and 18
// (JESD400-5), which the JEDEC profiles run at. Code 0 is the only one the
// standard defines -- 1.1 V for VDD and VDDQ, 1.8 V for VPP -- and the bench
// kit reads 0 in all three. A code it does not define shows as unknown rather
// than being taken for the default.
pub const DDR5_NOMINAL_VOLTAGES: [(u32, &[(i64, f64)]); 3] =
    [(16, &[(0, 1.10)]), (17, &[(0, 1.10)]), (18, &[(0, 1.80)])];

// What follows tAA, the same way in the base block, an XMP 3.0 profile and an
// EXPO profile: two bytes each, in picoseconds, except the three refresh
// cycle times, which are in nanoseconds. Read off the bench kit's base block
// as tWR 30 ns and tRFC1/2/sb 295/160/130 ns, the JEDEC figures for its 16Gb
// dies; its XMP and EXPO profiles carry the same values at the same distance.
pub const DDR5_TAA_FOLLOWERS: [(&str, u32); 5] = [("trcd", 2), ("trp", 4), ("tras", 6), ("trc", 8), ("twr", 10)];
pub const DDR5_TRFC_FOLLOWERS: [(&str, u32); 3] = [("trfc1", 12), ("trfc2", 14), ("trfcsb", 16)];

// Base-block timings stored as picoseconds and then a least number of clocks,
// three bytes each (JESD400-5): tRRD_L 5 ns / 8, tCCD_L 5 ns / 8, tFAW
// 13.333 ns / 32 and tRTP 7.5 ns / 12 on the bench kit.
//
// tCCD_L_WR at 76, tCCD_L_WTR (tWTR_L) at 85 and tCCD_S_WTR (tWTR_S) at 88,
// as i2c-tools' DDR5 decode-dimms reads them. The bench kit gives 20 ns / 32,
// 10 ns / 16 and 2.5 ns / 4 there, the JEDEC minimums for each. 79 between
// them is tCCD_L_WR2, which also reads 10 ns / 16 and is not shown.
pub const DDR5_FLOORED: [(&str, u32); 7] =
    [("trrd_l", 70), ("tccd_l", 73), ("tccd_l_wr", 76), ("tfaw", 82), ("twtr_l", 85), ("twtr_s", 88), ("trtp", 91)];

// The module's makeup (JESD400-5): the first SDRAM's density and dies per
// package, its I/O width, ranks per sub-channel and the bus. The bench kit
// reads 0x04, 0x20, 0x00 and 0x22: 16Gb x8 dies, one rank, two 32-bit
// sub-channels -- 16 GB, the size on its label.
pub const DDR5_DENSITY: u32 = 4;
pub const DDR5_IO_WIDTH: u32 = 6;
pub const DDR5_RANKS: u32 = 234;
pub const DDR5_BUS: u32 = 235;
pub const DDR5_DENSITY_GBIT: &[(i64, i64)] = &[(1, 4), (2, 8), (3, 12), (4, 16), (5, 24), (6, 32), (7, 48), (8, 64)];
pub const DDR5_DIES_PER_PACKAGE: &[(i64, i64)] = &[(0, 1), (2, 2), (3, 4), (4, 8), (5, 16)];
pub const DDR5_IO_WIDTHS: &[(i64, i64)] = &[(0, 4), (1, 8), (2, 16), (3, 32)];
pub const DDR5_ASYMMETRIC_RANKS: i64 = 0x40;

// The module's own devices: the SPD hub and the first PMIC, each a JEP106
// maker ID and then a type byte whose bit 7 says the device is fitted. The
// bench kit reads 0x80B3 (IDT, now Renesas) with 0x80, and 0x8A8C (Richtek)
// with 0x82; the second and third PMIC slots read zero.
pub const DDR5_SPD_HUB_MAKER: u32 = 194;
pub const DDR5_SPD_HUB_TYPE: u32 = 196;
pub const DDR5_PMIC_MAKER: u32 = 198;
pub const DDR5_PMIC_TYPE: u32 = 200;
pub const DDR5_DEVICE_FITTED: i64 = 0x80;

// The reference raw card: the JEDEC PCB design the module follows. Bits 4:0
// name the card, A upward, and bits 7:5 its revision; 0x1F in the low bits
// means the module follows no JEDEC design. The bench kit reads 0x00 -- card
// A, revision 0 -- which a reference tool reports the same way, and its height byte
// beside it (230, 0x11: 17 + 15 = 32 mm) agrees too, which is what places
// this block's offsets.
pub const DDR5_RAW_CARD: u32 = 232;
pub const DDR5_NO_REFERENCE_CARD: i64 = 0x1F;

// The build date, BCD, as the identity block reads it.
pub const DDR5_MFG_YEAR: u32 = 0x203;
pub const DDR5_MFG_WEEK: u32 = 0x204;

pub const XMP3_HEADER: u32 = 0x280;
pub const XMP3_PROFILES: [u32; 5] = [0x2C0, 0x300, 0x340, 0x380, 0x3C0];
// Within an XMP 3.0 profile: the voltages lead, then tCK, the CAS mask, and
// tAA onward in the same order as the base block.
pub const XMP3_VDD: u32 = 1;
pub const XMP3_TCK: u32 = 5;
pub const XMP3_CAS_MASK: u32 = 7;
pub const XMP3_TAA: u32 = 0x0D;
pub const XMP3_VPP: u32 = 0;
pub const XMP3_VDDQ: u32 = 2;
pub const EXPO_HEADER: u32 = 0x340;

// AMD EXPO: a ten-byte header, then 40-byte profiles, each VDD, VDDQ and VPP,
// a reserved byte, tCK, and tAA onward as the base block has it -- down to
// tRTP, since EXPO stores tRRD_L, tCCD_L, tFAW and tRTP as well, with no
// clock floors. Matches the OMI spdr decoder's layout, and the bench kit's
// profile at 0x34A reads 1.45 / 1.45 / 1.80 V, 250 ps (8000) and CL38-48-48,
// the same as its XMP profile. A profile is present when its tCK is not zero.
pub const EXPO_PROFILES: [u32; 2] = [0x34A, 0x372];
pub const EXPO_VDD: u32 = 0;
pub const EXPO_VDDQ: u32 = 1;
pub const EXPO_VPP: u32 = 2;
pub const EXPO_TCK: u32 = 4;
pub const EXPO_TAA: u32 = 6;
pub const EXPO_FLOORED: [(&str, u32); 4] = [("trrd_l", 18), ("tccd_l", 20), ("tfaw", 26), ("trtp", 32)];

fn word(values: &SpdBytes, offset: u32) -> i64 {
    byte(values, offset) | (byte(values, offset + 1) << 8)
}

/// JESD400-5 rounding: nCK = trunc((t x 0.997 / tCK) + 1).
fn ddr5_cycles(time_ps: i64, tck_ps: i64) -> Option<i64> {
    if time_ps == 0 || tck_ps == 0 {
        return None;
    }
    Some(((time_ps * 997).div_euclid(tck_ps) + 1000).div_euclid(1000))
}

fn ddr5_supported_cls(values: &SpdBytes, start: u32) -> Vec<i64> {
    let mut result = Vec::new();
    for byte_index in 0..5 {
        let bits = byte(values, start + byte_index);
        for bit in 0..8 {
            if bits & (1 << bit) != 0 {
                result.push(20 + 2 * (byte_index as i64 * 8 + bit));
            }
        }
    }
    result
}

/// The standard data rate for a tCK, or the raw figure off the table.
fn ddr5_rate(tck_ps: i64) -> i64 {
    for &(rate, bin_tck) in DDR5_SPEED_BINS {
        if (bin_tck - tck_ps).abs() <= 1 {
            return rate;
        }
    }
    pyfmt::round_int(2_000_000.0 / tck_ps as f64 / 100.0) * 100
}

/// (VDD, VDDQ, VPP) from the base block, `None` for a code not defined.
fn ddr5_nominal_voltages(values: &SpdBytes) -> [Option<f64>; 3] {
    DDR5_NOMINAL_VOLTAGES.map(|(offset, table)| table_get(table, byte(values, offset) >> 4))
}

fn volts(value: Option<f64>) -> String {
    match value {
        None => EM_DASH.to_string(),
        Some(value) => format!("{value:.2} V"),
    }
}

/// A clock count for the table, or a dash where the profile has none.
fn shown(cycles: Option<i64>) -> SpdValue {
    match cycles {
        None => SpdValue::text(EM_DASH),
        Some(cycles) => SpdValue::Int(cycles),
    }
}

/// The base block's least clock counts for its three-byte timings.
fn ddr5_floors(values: &SpdBytes) -> BTreeMap<&'static str, i64> {
    DDR5_FLOORED.iter().map(|&(key, offset)| (key, byte(values, offset + 2))).collect()
}

/// One table column: timings in clocks at `tck_ps`.
///
/// `taa` is where tAA sits, with the rest of DDR5_TAA_FOLLOWERS and
/// DDR5_TRFC_FOLLOWERS after it. `floored` gives where the picosecond word of
/// tRRD_L, tCCD_L, tFAW and tRTP sits in this profile, or `None` where the
/// profile has no copy of them; `floors` the least clocks each may be.
/// `voltages` is (VDD, VDDQ, VPP), each `None` where it is not known.
#[allow(clippy::too_many_arguments)]
fn ddr5_profile(
    name: String,
    values: &SpdBytes,
    taa: u32,
    tck_ps: i64,
    voltages: [Option<f64>; 3],
    supported: &[i64],
    floored: Option<&[(&str, u32)]>,
    floors: Option<&BTreeMap<&'static str, i64>>,
) -> Option<SpdProfile> {
    let mut cl = ddr5_cycles(word(values, taa), tck_ps)?;
    // CAS latency only comes in the values the module lists as supported.
    if !supported.is_empty() {
        cl = supported.iter().copied().find(|&value| value >= cl).unwrap_or(cl);
    }
    let rate = ddr5_rate(tck_ps);
    let [vdd, vddq, vpp] = voltages;
    let mut profile = SpdProfile::from([
        ("name", SpdValue::Text(name)),
        // Half the data rate, as the speed is named: 416 ps is DDR5-4800 and
        // 2400 MHz, not the 2404 its rounded tCK works out to.
        ("frequency", SpdValue::Text(format!("{} MHz", rate.div_euclid(2)))),
        ("data_rate", SpdValue::Text(format!("DDR5-{rate}"))),
        ("cl", SpdValue::Int(cl)),
        ("voltage", SpdValue::Text(volts(vdd))),
        ("vdd", SpdValue::Text(volts(vdd))),
        ("vddq", SpdValue::Text(volts(vddq))),
        ("vpp", SpdValue::Text(volts(vpp))),
    ]);
    for (key, step) in DDR5_TAA_FOLLOWERS {
        profile.insert(key, shown(ddr5_cycles(word(values, taa + step), tck_ps)));
    }
    for (key, step) in DDR5_TRFC_FOLLOWERS {
        profile.insert(key, shown(ddr5_cycles(word(values, taa + step) * 1000, tck_ps)));
    }
    for (key, _) in DDR5_FLOORED {
        let offset = floored.and_then(|floored| table_get(floored, key));
        let mut cycles = offset.and_then(|offset| ddr5_cycles(word(values, offset), tck_ps));
        if let Some(found) = cycles {
            cycles = Some(found.max(floors.and_then(|f| f.get(key).copied()).unwrap_or(0)));
        }
        profile.insert(key, shown(cycles));
    }
    Some(profile)
}

/// Module size, ranks and DRAM organization from the base block.
fn ddr5_makeup(values: &SpdBytes) -> SpdFields {
    let density = table_get(DDR5_DENSITY_GBIT, byte(values, DDR5_DENSITY) & 0x1F);
    let dies = table_get(DDR5_DIES_PER_PACKAGE, (byte(values, DDR5_DENSITY) >> 5) & 0x07);
    let width = table_get(DDR5_IO_WIDTHS, (byte(values, DDR5_IO_WIDTH) >> 5) & 0x07);
    let ranks_byte = byte(values, DDR5_RANKS);
    let ranks = ((ranks_byte >> 3) & 0x07) + 1;
    let bus = byte(values, DDR5_BUS);
    let bus_width = if (bus & 0x07) <= 3 { 8 << (bus & 0x07) } else { 0 };
    let sub_channels = ((bus >> 5) & 0x03) + 1;
    let mut found = SpdFields::new();
    if let (Some(density), Some(dies), Some(width)) = (density, dies, width) {
        let dies_text = if dies > 1 { format!(" ({dies} dies)") } else { String::new() };
        found.insert("organization".into(), SpdValue::Text(format!("{density}Gb x{width}{dies_text}")));
        found.insert("rank".into(), SpdValue::Text(format!("{ranks}R")));
        // A module mixing two kinds of rank describes the second elsewhere;
        // its size is left to the inventory rather than half computed here.
        if bus_width != 0 && ranks_byte & DDR5_ASYMMETRIC_RANKS == 0 {
            let gigabits = sub_channels * (bus_width / width) * density * dies * ranks;
            found.insert("capacity".into(), SpdValue::Text(format!("{} GB", gigabits.div_euclid(8))));
        }
    }
    found
}

/// The maker of a device the module says is fitted, or a dash.
fn ddr5_device_maker(values: &SpdBytes, maker: u32, device_type: u32) -> String {
    if byte(values, device_type) & DDR5_DEVICE_FITTED == 0 {
        return EM_DASH.to_string();
    }
    decode_jep106_id(Some(byte(values, maker) as u8), Some(byte(values, maker + 1) as u8))
}

/// The reference raw card as "A Rev 0", or a dash where there is none.
///
/// Only A to Z are named. The five codes above Z are left as their number
/// rather than guessed at, since nothing on this bench shows what a module
/// that uses one prints for it.
fn ddr5_raw_card(values: &SpdBytes) -> String {
    let value = byte(values, DDR5_RAW_CARD);
    let (card, revision) = (value & 0x1F, value >> 5);
    if card == DDR5_NO_REFERENCE_CARD {
        return "None (not a JEDEC design)".to_string();
    }
    let name = if card < 26 { ((b'A' + card as u8) as char).to_string() } else { format!("code 0x{card:02X}") };
    format!("{name} Rev {revision}")
}

/// The build date as "Week 4, 2025", or `None` where it is not set.
fn ddr5_week(values: &SpdBytes) -> Option<String> {
    let date =
        decode_manufacture_date(Some(byte(values, DDR5_MFG_YEAR) as u8), Some(byte(values, DDR5_MFG_WEEK) as u8));
    if date == EM_DASH {
        return None;
    }
    let (week, year) = date.split_once(" / ")?;
    Some(format!("Week {}, {}", week.parse::<i64>().ok()?, year))
}

/// Decode the module type and the fastest three JEDEC speed bins; `None` when
/// the bytes are not DDR5's.
pub fn decode_ddr5_base(values: &SpdBytes) -> Result<Option<SpdRecord>, DecodeError> {
    if byte(values, 2) != DDR5 as i64 {
        return Ok(None);
    }
    let tck_min = word(values, DDR5_TCK_MIN);
    let supported = ddr5_supported_cls(values, DDR5_CAS_MASK);
    let taa = word(values, DDR5_TAA);

    let mut profiles = Vec::new();
    let floors = ddr5_floors(values);
    let nominal = ddr5_nominal_voltages(values);
    if tck_min != 0 {
        // From the module's own top bin down, each speed the standard defines
        // that it can run, as CPU-Z and Thaiphoon list them.
        let mut bins = DDR5_SPEED_BINS.to_vec();
        bins.sort_by(|a, b| b.cmp(a));
        for (rate, bin_tck) in bins {
            if bin_tck < tck_min - 1 {
                continue;
            }
            let tck = bin_tck.max(tck_min);
            if let Some(&highest) = supported.last() {
                match ddr5_cycles(taa, tck) {
                    None => return Err("'>' not supported between instances of 'NoneType' and 'int'".into()),
                    Some(cycles) if cycles > highest => continue,
                    Some(_) => {}
                }
            }
            let profile = ddr5_profile(
                format!("JEDEC {rate}"),
                values,
                DDR5_TAA,
                tck,
                nominal,
                &supported,
                Some(&DDR5_FLOORED),
                Some(&floors),
            );
            if let Some(profile) = profile {
                profiles.push(profile);
            }
            if profiles.len() == 3 {
                break;
            }
        }
    }

    let module_code = (byte(values, 3) & 0x0F) as u8;
    let mut record = SpdRecord { fields: ddr5_makeup(values), profiles };
    record.set("pmic", ddr5_device_maker(values, DDR5_PMIC_MAKER, DDR5_PMIC_TYPE));
    record.set("spd_hub", ddr5_device_maker(values, DDR5_SPD_HUB_MAKER, DDR5_SPD_HUB_TYPE));
    record.set("raw_card", ddr5_raw_card(values));
    if let Some(week) = ddr5_week(values) {
        record.set("manufacture_date", week);
    }
    record.set("memory_type", "DDR5");
    record.set(
        "module_type",
        table_get(DDR5_MODULE_TYPES, module_code)
            .map(str::to_string)
            .unwrap_or_else(|| format!("Type 0x{module_code:X}")),
    );
    record.set(
        "max_bandwidth",
        if tck_min != 0 {
            format!("DDR5-{} ({} MHz)", ddr5_rate(tck_min), ddr5_rate(tck_min).div_euclid(2))
        } else {
            EM_DASH.to_string()
        },
    );
    Ok(Some(record))
}

/// Bits 6:5 are whole volts, bits 4:0 count 0.05 V: 0x29 is 1.45 V.
fn xmp3_voltage(value: i64) -> f64 {
    let value = value & 0xFF;
    ((value >> 5) & 0x03) as f64 + (value & 0x1F) as f64 * 0.05
}

/// Decode the enabled Intel XMP 3.0 profiles (and an EXPO block beside them).
pub fn decode_ddr5_xmp(values: &SpdBytes) -> XmpDecode {
    let mut extensions = Vec::new();
    let mut profiles = Vec::new();
    if (byte(values, XMP3_HEADER), byte(values, XMP3_HEADER + 1)) == (0x0C, 0x4A) {
        let revision = byte(values, XMP3_HEADER + 2);
        extensions.push(format!("XMP {}.{}", revision >> 4, revision & 0x0F));
        let enabled = byte(values, XMP3_HEADER + 3);
        for (index, base) in XMP3_PROFILES.into_iter().enumerate() {
            if enabled & (1 << index) == 0 {
                continue;
            }
            let tck = word(values, base + XMP3_TCK);
            if tck == 0 {
                continue;
            }
            // The profile's own tRRD_L, tCCD_L, tFAW and tRTP are not read:
            // the bench kit leaves them zero, so their place in the profile
            // has nothing to be checked against, and those rows show a dash.
            let profile = ddr5_profile(
                format!("XMP-{}", ddr5_rate(tck)),
                values,
                base + XMP3_TAA,
                tck,
                [XMP3_VDD, XMP3_VDDQ, XMP3_VPP].map(|offset| Some(xmp3_voltage(byte(values, base + offset)))),
                &ddr5_supported_cls(values, base + XMP3_CAS_MASK),
                None,
                None,
            );
            if let Some(profile) = profile {
                profiles.push(profile);
            }
        }
    }
    // An EXPO block shares the extension area on kits sold for both
    // platforms, and its profiles are shown beside XMP's.
    let mut expo = Vec::new();
    if (0..4).map(|i| byte(values, EXPO_HEADER + i) as u8).collect::<Vec<u8>>() == b"EXPO" {
        extensions.push("EXPO".to_string());
        let floors = ddr5_floors(values);
        for base in EXPO_PROFILES {
            let tck = word(values, base + EXPO_TCK);
            if tck == 0 {
                continue;
            }
            let taa = base + EXPO_TAA;
            let floored: Vec<(&str, u32)> = EXPO_FLOORED.iter().map(|&(key, step)| (key, taa + step)).collect();
            let profile = ddr5_profile(
                format!("EXPO-{}", ddr5_rate(tck)),
                values,
                taa,
                tck,
                [EXPO_VDD, EXPO_VDDQ, EXPO_VPP].map(|offset| Some(xmp3_voltage(byte(values, base + offset)))),
                &[],
                Some(&floored),
                Some(&floors),
            );
            if let Some(profile) = profile {
                expo.push(profile);
            }
        }
    }
    XmpDecode {
        extension: if extensions.is_empty() { EM_DASH.to_string() } else { extensions.join(", ") },
        profiles,
        expo_profiles: expo,
    }
}

/// One complete DDR5 SPD display record, or `None` when the bytes are not
/// DDR5's. `identity` is merged over the decoded fields.
pub fn decode_ddr5_spd(values: &SpdBytes, identity: Option<&SpdFields>) -> Result<Option<SpdRecord>, DecodeError> {
    let Some(mut record) = decode_ddr5_base(values)? else {
        return Ok(None);
    };
    let xmp = decode_ddr5_xmp(values);
    record.set("extension", xmp.extension);
    // Five columns in the table. XMP and EXPO are the reason anybody opens
    // this page, so they keep their columns and JEDEC gives way.
    let mut tuned = xmp.profiles;
    tuned.extend(xmp.expo_profiles);
    let keep = 5usize.saturating_sub(tuned.len());
    let mut columns: Vec<SpdProfile> = record.profiles.into_iter().take(keep).collect();
    columns.extend(tuned);
    columns.truncate(5);
    record.profiles = columns;
    if let Some(identity) = identity {
        record.update(identity);
    }
    Ok(Some(record))
}

fn is_blank(value: Option<&SpdValue>) -> bool {
    match value {
        None | Some(SpdValue::None) => true,
        Some(SpdValue::Text(text)) => text.is_empty() || text == EM_DASH,
        Some(SpdValue::Int(_)) => false,
    }
}

/// Attach SMBIOS physical slot labels without guessing from addresses.
pub fn join_slots(mut modules: Vec<SpdRecord>, inventory: &[DimmModule]) -> Vec<SpdRecord> {
    let mut remaining: Vec<&DimmModule> = inventory.iter().collect();
    remaining.sort_by_key(|item| item.slot.clone().unwrap_or_default());
    for module in modules.iter_mut() {
        let mut matched: Option<usize> = None;
        let serial = module.text("serial_number").unwrap_or_default().trim().to_string();
        if !(serial.is_empty() || serial == "0" || serial == EM_DASH) {
            matched = remaining.iter().position(|item| item.serial_number.trim() == serial);
        }
        if matched.is_none() {
            let part = module.text("part_number").unwrap_or_default().trim().to_uppercase();
            matched = remaining.iter().position(|item| item.part_number.trim().to_uppercase() == part);
        }
        if matched.is_none() && !remaining.is_empty() {
            matched = Some(0);
        }
        if let Some(index) = matched {
            let found = remaining.remove(index);
            let slot = found.slot.clone().filter(|s| !s.is_empty()).unwrap_or_else(|| found.device_locator.clone());
            module.set("slot", slot);
            for (field, value) in [("capacity", &found.capacity), ("rank", &found.rank)] {
                if is_blank(module.get(field)) {
                    module.set(field, if value.is_empty() { EM_DASH.to_string() } else { value.clone() });
                }
            }
            // The inventory writes its size "16GB", for the compact module
            // selector; on this page a DDR5 size reads as the decoded ones and
            // the system Capacity above it do, "16 GB".
            if module.get("memory_type").and_then(SpdValue::as_str) == Some("DDR5")
                && module.get("capacity").and_then(SpdValue::as_str) == Some(found.capacity.as_str())
                && found.capacity_gb != 0
            {
                module.set("capacity", format!("{} GB", found.capacity_gb));
            }
            let module_vendor = module.text("module_manufacturer").unwrap_or_default();
            if module_vendor.is_empty() || module_vendor == EM_DASH || module_vendor.to_lowercase().starts_with("0x") {
                let replacement = if !found.module_manufacturer.is_empty() {
                    found.module_manufacturer.clone()
                } else if !module_vendor.is_empty() {
                    module_vendor
                } else {
                    EM_DASH.to_string()
                };
                module.set("module_manufacturer", replacement);
            }
            // Do not fill the DRAM maker or die from the inventory's
            // part-number lookup. SPD is a readout page: if those bytes do
            // not identify the component, preserve the unavailable/raw value
            // instead of substituting a result inferred from a known kit.
        }
        if !module.fields.contains_key("slot") {
            let address = module.get("address").and_then(SpdValue::as_int).unwrap_or(0);
            module.set("slot", format!("SPD 0x{address:02X}"));
        }
    }
    modules
}

/// Read installed SPD records once and return display-ready modules.
///
/// An error is a decode the Python raised out of; the SPD page reports it.
pub fn read_spd_modules(reader_factory: Option<ReaderFactory>, refresh: bool) -> Result<Vec<SpdRecord>, DecodeError> {
    use super::ddr_generation::{DdrGeneration, detect_ddr_generation};
    use super::dimm_inventory::read_modules;

    let generation = detect_ddr_generation();
    let mut modules = Vec::new();
    if generation == DdrGeneration::Ddr4 {
        modules = super::ddr4_spd::read_modules(reader_factory, refresh);
    } else if generation == DdrGeneration::Ddr5 {
        use super::ddr5_spd::{read_identity, read_spd_bytes};

        for identity in read_identity(reader_factory, refresh, Some(DdrGeneration::Ddr5)) {
            let mut module = SpdRecord { fields: identity.fields(), profiles: Vec::new() };
            for key in [
                "module_type",
                "capacity",
                "rank",
                "max_bandwidth",
                "extension",
                "organization",
                "pmic",
                "spd_hub",
                "raw_card",
            ] {
                module.set(key, EM_DASH);
            }
            module.set("memory_type", "DDR5");
            let values = read_spd_bytes(Some(identity.address), Some(identity.controller), reader_factory);
            // Identity is already decoded from the same read; only the
            // module description and the timing columns come from here.
            let decoded = if values.is_empty() { None } else { decode_ddr5_spd(&values, None)? };
            if let Some(decoded) = decoded {
                module.update(&decoded.fields);
                module.profiles = decoded.profiles;
            }
            modules.push(module);
        }
    }
    let inventory = read_modules(refresh);
    Ok(join_slots(modules, &inventory))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::memory::ddr5_telemetry::{SmbusError, SmbusReader};
    use std::sync::Mutex;

    fn show(value: &SpdValue) -> String {
        value.display().unwrap_or_else(|| "None".into())
    }

    // --- tests/test_spd_profiles.py

    pub(crate) fn sample_spd() -> SpdBytes {
        let mut values: SpdBytes = (0..487).map(|offset| (offset, 0)).collect();
        values.extend([
            (2, 0x0C),  // DDR4
            (3, 0x02),  // UDIMM
            (4, 0x05),  // 8 Gbit SDRAM
            (12, 0x09), // 2 ranks, x8 devices
            (13, 0x03), // 64-bit bus
            (18, 0x08),
            (20, 0x80), // CL14
            (21, 0x03), // CL15, CL16
            (24, 0x6C), // 13.5 ns tAA
            (25, 0x70),
            (26, 0x70),
            (27, 0x11),
            (28, 0x20),
            (29, 0x90),
            (125, 0xC2), // 8*125-62 = 938 ps, DDR4-2133
            (384, 0x0C),
            (385, 0x4A),
            (386, 0x01),
            (387, 0x20),
            (393, 0xA3), // 1.35 V
            (396, 0x05), // 625 ps, DDR4-3200
            (397, 0x80), // CL14 supported
            (401, 0x46), // 8.75 ns => CL14
            (402, 0x46),
            (403, 0x46),
            (404, 0x00),
            (405, 0xAA), // 21.25 ns => 34 clocks
            (406, 0xF0), // 30 ns => 48 clocks
        ]);
        values
    }

    fn fields(pairs: &[(&str, SpdValue)]) -> SpdFields {
        pairs.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()
    }

    #[test]
    fn base_module_geometry_and_bandwidth() {
        let decoded = decode_ddr4_base(&sample_spd()).unwrap().unwrap();
        assert_eq!(decoded.text("module_type").as_deref(), Some("UDIMM"));
        assert_eq!(decoded.text("capacity").as_deref(), Some("16 GB"));
        assert_eq!(decoded.text("rank").as_deref(), Some("2R"));
        assert_eq!(decoded.text("max_bandwidth").as_deref(), Some("DDR4-2133 (1066 MHz)"));
        assert_eq!(decoded.profiles.iter().map(|p| show(&p["cl"])).collect::<Vec<_>>(), vec!["14", "15", "16"]);
    }

    #[test]
    fn xmp_20_profile() {
        let decoded = decode_ddr4_xmp(&sample_spd()).unwrap();
        assert_eq!(decoded.extension, "XMP 2.0");
        assert_eq!(decoded.profiles.len(), 1);
        let profile = &decoded.profiles[0];
        assert_eq!(show(&profile["name"]), "XMP-3200");
        assert_eq!(show(&profile["frequency"]), "1600 MHz");
        assert_eq!(["cl", "trcd", "trp", "tras", "trc"].map(|key| show(&profile[key])), ["14", "14", "14", "34", "48"]);
        assert_eq!(show(&profile["voltage"]), "1.35 V");
    }

    #[test]
    fn identity_and_profiles_are_combined() {
        let decoded =
            decode_ddr4_spd(&sample_spd(), Some(&fields(&[("part_number", "TEST-3200".into())]))).unwrap().unwrap();
        assert_eq!(decoded.text("part_number").as_deref(), Some("TEST-3200"));
        assert_eq!(decoded.profiles.len(), 4);
    }

    #[test]
    fn non_ddr4_data_is_rejected() {
        let mut values = sample_spd();
        values.insert(2, 0x12);
        assert_eq!(decode_ddr4_spd(&values, None), Ok(None));
    }

    #[test]
    fn slot_join_does_not_infer_dram_identity_from_part_number() {
        let module = SpdRecord {
            fields: fields(&[
                ("part_number", "KNOWN-KIT".into()),
                ("serial_number", "1".into()),
                ("dram_manufacturer", "—".into()),
                ("dram_die", "—".into()),
            ]),
            profiles: Vec::new(),
        };
        let inventory = [DimmModule {
            part_number: "KNOWN-KIT".into(),
            serial_number: "1".into(),
            slot: Some("A2".into()),
            ic: "Samsung B-die".into(),
            ..DimmModule::default()
        }];
        let joined = join_slots(vec![module], &inventory).remove(0);
        assert_eq!(joined.text("slot").as_deref(), Some("A2"));
        assert_eq!(joined.text("dram_manufacturer").as_deref(), Some("—"));
        assert_eq!(joined.text("dram_die").as_deref(), Some("—"));
    }

    #[test]
    fn a_ddr5_size_from_the_inventory_reads_like_the_rest() {
        let join = |memory_type: &str| {
            let module = SpdRecord {
                fields: fields(&[
                    ("part_number", "KIT".into()),
                    ("serial_number", "1".into()),
                    ("memory_type", memory_type.into()),
                    ("capacity", "—".into()),
                ]),
                profiles: Vec::new(),
            };
            let inventory = [DimmModule {
                part_number: "KIT".into(),
                serial_number: "1".into(),
                slot: Some("A1".into()),
                capacity: "16GB".into(),
                capacity_gb: 16,
                ..DimmModule::default()
            }];
            join_slots(vec![module], &inventory).remove(0).text("capacity").unwrap()
        };
        assert_eq!(join("DDR5"), "16 GB");
        assert_eq!(join("DDR4"), "16GB");
    }

    #[test]
    fn reader_skips_reserved_spd_ranges() {
        // The Python patched ddr4_spd.read_identity and the backend; the
        // identity pass's result is handed straight to the part of
        // ddr4_spd::read_modules that reads the profile bytes, with the
        // addresses the no-backend path uses.
        struct Reader {
            ranges: Mutex<Vec<(i64, i64)>>,
        }

        impl SmbusReader for Reader {
            fn is_driver_open(&self) -> bool {
                true
            }

            fn read_ddr4_spd(
                &self,
                _address: u8,
                offset: i64,
                length: i64,
                _controller: u8,
            ) -> Result<SpdBytes, SmbusError> {
                self.ranges.lock().unwrap().push((offset, length));
                let source = sample_spd();
                Ok((offset..offset + length)
                    .filter_map(|position| source.get(&(position as u32)).map(|v| (position as u32, *v)))
                    .collect())
            }
        }

        let reader = Reader { ranges: Mutex::new(Vec::new()) };
        let identity = [crate::memory::ddr5_spd::SpdIdentity {
            address: 0x51,
            controller: 0,
            part_number: "TEST-3200".into(),
            ..Default::default()
        }];
        use crate::intel::intel_pch_smbus::{CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES};
        let found =
            crate::memory::ddr4_spd::collect_modules(&reader, &CONTROLLER_OFFSETS, &SPD_HUB_ADDRESSES, &identity)
                .unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(*reader.ranges.lock().unwrap(), vec![(2, 28), (120, 6), (384, 6), (393, 14), (427, 5)]);
    }

    // --- tests/test_ddr5_spd_profiles.py
    //
    // DDR5 SPD: the JEDEC base block and Intel XMP 3.0, from a real module.
    //
    // The bytes are the base block and the extension area of a V-Color
    // TMXFL1680838KWK (DDR5-8000 CL38, 1.45 V) read on an LGA1700 bench. The
    // identity block at 0x200 is left out: it carries the module's serial
    // number. Rows of zeros are omitted.

    const ROWS: &[(u32, &str)] = &[
        (0x000, "30 10 12 02 04 00 20 62 00 00 00 00 10 02 05 00"),
        (0x010, "00 00 00 00 A0 01 F2 03 7A 0D 00 00 00 00 80 3E"),
        (0x020, "80 3E 80 3E 00 7D 80 BB 30 75 27 01 A0 00 82 00"),
        (0x040, "00 00 00 00 00 00 88 13 08 88 13 08 20 4E 20 10"),
        (0x050, "27 10 15 34 20 10 27 10 C4 09 04 4C 1D 0C 00 00"),
        (0x0C0, "10 00 80 B3 80 21 8A 8C 82 20 00 00 00 00 00 00"),
        (0x0E0, "00 00 00 00 00 00 00 00 00 00 00 22 00 00 00 00"),
        (0x280, "0C 4A 30 01 15 8A 8C 01 07 00 00 00 00 00 00 00"),
        (0x2B0, "00 00 00 00 00 00 00 00 00 00 00 00 00 00 5B 11"),
        (0x2C0, "30 29 29 00 22 FA 00 FA EF 02 00 00 00 1C 25 E0"),
        (0x2D0, "2E E0 2E 00 7D E0 AB 30 75 27 01 A0 00 82 00 00"),
        (0x2F0, "00 00 00 00 00 00 00 00 00 00 00 00 00 00 79 89"),
        (0x340, "45 58 50 4F 11 03 01 01 00 00 29 29 30 00 FA 00"),
        (0x350, "1C 25 E0 2E E0 2E 00 7D E0 AB 30 75 27 01 A0 00"),
        (0x360, "82 00 88 13 88 13 20 4E 80 3E CD 37 10 27 C4 09"),
        (0x370, "4C 1D 00 00 00 00 00 00 00 00 00 00 00 00 00 00"),
        (0x3B0, "00 00 00 00 00 00 00 00 00 00 00 00 00 00 D8 A7"),
    ];

    fn module_bytes() -> SpdBytes {
        let mut values: SpdBytes = (0..0x400).map(|offset| (offset, 0)).collect();
        for (base, row) in ROWS {
            for (index, text) in row.split_whitespace().enumerate() {
                values.insert(base + index as u32, u8::from_str_radix(text, 16).unwrap());
            }
        }
        // The build date from the identity block, without the serial beside it.
        values.insert(0x203, 0x25);
        values.insert(0x204, 0x04);
        values
    }

    const ROWS_SHOWN: [&str; 17] = [
        "frequency",
        "cl",
        "trcd",
        "trp",
        "tras",
        "trc",
        "twr",
        "trfc1",
        "trfc2",
        "trfcsb",
        "trrd_l",
        "tccd_l",
        "tfaw",
        "trtp",
        "vdd",
        "vddq",
        "vpp",
    ];

    fn column<'a>(record: &'a SpdRecord, name: &str) -> &'a SpdProfile {
        record.profiles.iter().find(|p| show(&p["name"]) == name).unwrap()
    }

    fn ddr5_record(values: &SpdBytes) -> SpdRecord {
        decode_ddr5_spd(values, None).unwrap().unwrap()
    }

    #[test]
    fn module_description() {
        let record = decode_ddr5_base(&module_bytes()).unwrap().unwrap();
        assert_eq!(record.text("memory_type").as_deref(), Some("DDR5"));
        assert_eq!(record.text("module_type").as_deref(), Some("UDIMM"));
        assert_eq!(record.text("max_bandwidth").as_deref(), Some("DDR5-4800 (2400 MHz)"));
    }

    #[test]
    fn the_three_fastest_jedec_bins() {
        let record = decode_ddr5_base(&module_bytes()).unwrap().unwrap();
        let rows: Vec<Vec<String>> = record
            .profiles
            .iter()
            .map(|p| ["name", "frequency", "cl", "trcd", "trp", "tras", "trc", "voltage"].map(|k| show(&p[k])).to_vec())
            .collect();
        assert_eq!(
            rows,
            vec![
                vec!["JEDEC 4800", "2400 MHz", "40", "39", "39", "77", "116", "1.10 V"],
                vec!["JEDEC 4400", "2200 MHz", "36", "36", "36", "71", "106", "1.10 V"],
                vec!["JEDEC 4000", "2000 MHz", "32", "32", "32", "64", "96", "1.10 V"],
            ]
        );
    }

    #[test]
    fn not_ddr5_is_not_decoded() {
        let mut values = module_bytes();
        values.insert(2, 0x0C);
        assert_eq!(decode_ddr5_base(&values), Ok(None));
    }

    #[test]
    fn profile_one_is_the_kits_rating() {
        let xmp = decode_ddr5_xmp(&module_bytes());
        assert_eq!(xmp.profiles.len(), 1);
        let profile = &xmp.profiles[0];
        assert_eq!(show(&profile["name"]), "XMP-8000");
        assert_eq!(show(&profile["frequency"]), "4000 MHz");
        assert_eq!(["cl", "trcd", "trp", "tras", "trc"].map(|k| show(&profile[k])), ["38", "48", "48", "128", "176"]);
        assert_eq!(show(&profile["voltage"]), "1.45 V");
    }

    #[test]
    fn extension_names_both_blocks() {
        assert_eq!(decode_ddr5_xmp(&module_bytes()).extension, "XMP 3.0, EXPO");
    }

    #[test]
    fn disabled_profiles_are_not_read() {
        // Profile 3's slot at 0x340 holds this kit's EXPO block. With only
        // profile 1 enabled it must not be read as a second XMP profile.
        let names: Vec<String> = decode_ddr5_xmp(&module_bytes()).profiles.iter().map(|p| show(&p["name"])).collect();
        assert_eq!(names, vec!["XMP-8000"]);
    }

    #[test]
    fn no_xmp_header_means_no_xmp() {
        let mut values = module_bytes();
        values.insert(0x280, 0);
        assert_eq!(decode_ddr5_xmp(&values).extension, "EXPO");
        assert_eq!(decode_ddr5_xmp(&values).profiles, vec![]);
    }

    #[test]
    fn jedec_then_xmp_in_five_columns() {
        let record = decode_ddr5_spd(&module_bytes(), Some(&fields(&[("part_number", "X".into())]))).unwrap().unwrap();
        assert_eq!(
            record.profiles.iter().map(|p| show(&p["name"])).collect::<Vec<_>>(),
            vec!["JEDEC 4800", "JEDEC 4400", "JEDEC 4000", "XMP-8000", "EXPO-8000"]
        );
        assert_eq!(record.text("part_number").as_deref(), Some("X"));
    }

    #[test]
    fn every_row_of_every_column() {
        // Counts use the JESD400-5 rounding, whose 0.3% allowance is why
        // tRFC2's 160 ns at 250 ps comes to 639 rather than 640. XMP leaves
        // its own tRRD_L, tCCD_L, tFAW and tRTP zero on this kit.
        let record = ddr5_record(&module_bytes());
        let table: BTreeMap<String, Vec<String>> = record
            .profiles
            .iter()
            .map(|p| (show(&p["name"]), ROWS_SHOWN.iter().map(|k| show(&p[k])).collect()))
            .collect();
        let row = |cells: &str| cells.split(',').map(|c| c.trim().to_string()).collect::<Vec<String>>();
        let expected = BTreeMap::from([
            (
                "JEDEC 4800".to_string(),
                row("2400 MHz, 40, 39, 39, 77, 116, 72, 708, 384, 312, 12, 12, 32, 18, 1.10 V, 1.10 V, 1.80 V"),
            ),
            (
                "JEDEC 4400".to_string(),
                row("2200 MHz, 36, 36, 36, 71, 106, 66, 648, 352, 286, 11, 11, 32, 17, 1.10 V, 1.10 V, 1.80 V"),
            ),
            (
                "JEDEC 4000".to_string(),
                row("2000 MHz, 32, 32, 32, 64, 96, 60, 589, 320, 260, 10, 10, 32, 15, 1.10 V, 1.10 V, 1.80 V"),
            ),
            (
                "XMP-8000".to_string(),
                row("4000 MHz, 38, 48, 48, 128, 176, 120, 1177, 639, 519, —, —, —, —, 1.45 V, 1.45 V, 1.80 V"),
            ),
            (
                "EXPO-8000".to_string(),
                row("4000 MHz, 38, 48, 48, 128, 176, 120, 1177, 639, 519, 20, 20, 57, 30, 1.45 V, 1.45 V, 1.80 V"),
            ),
        ]);
        assert_eq!(table, expected);
    }

    #[test]
    fn the_module_makeup_and_its_devices() {
        let record = ddr5_record(&module_bytes());
        assert_eq!(record.text("capacity").as_deref(), Some("16 GB"));
        assert_eq!(record.text("organization").as_deref(), Some("16Gb x8"));
        assert_eq!(record.text("rank").as_deref(), Some("1R"));
        assert_eq!(record.text("pmic").as_deref(), Some("Richtek"));
        assert_eq!(record.text("spd_hub").as_deref(), Some("Renesas (IDT)"));
        assert_eq!(record.text("manufacture_date").as_deref(), Some("Week 4, 2025"));
    }

    #[test]
    fn jedec_voltages_are_the_modules_nominal_ones() {
        // Bytes 16-18 read 0 on this kit: 1.1 V, 1.1 V and 1.8 V. A code the
        // standard does not define is not taken for the default.
        let mut values = module_bytes();
        values.insert(17, 0x10);
        let record = ddr5_record(&values);
        let jedec = &record.profiles[0];
        assert_eq!(["vdd", "vddq", "vpp"].map(|k| show(&jedec[k])), ["1.10 V", "—", "1.80 V"]);
    }

    #[test]
    fn a_device_not_fitted_is_not_named() {
        let mut values = module_bytes();
        values.insert(200, 0x00);
        assert_eq!(ddr5_record(&values).text("pmic").as_deref(), Some("—"));
    }

    #[test]
    fn an_expo_block_without_profiles_adds_no_column() {
        let mut values = module_bytes();
        values.insert(0x34E, 0);
        values.insert(0x34F, 0);
        let names: Vec<String> = ddr5_record(&values).profiles.iter().map(|p| show(&p["name"])).collect();
        assert!(!names.contains(&"EXPO-8000".to_string()));
        assert_eq!(decode_ddr5_xmp(&values).extension, "XMP 3.0, EXPO");
    }

    #[test]
    fn the_jedec_columns_carry_tccd_l_wr_and_both_twtr() {
        // The bench kit's base block: 20 ns / 32 at byte 76, 10 ns / 16 at
        // 85 and 2.5 ns / 4 at 88 -- 48, 24 and 6 clocks at DDR5-4800.
        let record = ddr5_record(&module_bytes());
        let jedec = column(&record, "JEDEC 4800");
        assert_eq!(["tccd_l_wr", "twtr_l", "twtr_s"].map(|k| show(&jedec[k])), ["48", "24", "6"]);
    }

    #[test]
    fn a_slower_bin_counts_fewer_clocks_but_not_below_the_floor() {
        // At DDR5-4000 (500 ps) 2.5 ns is 5 clocks, above tWTR_S's floor of
        // 4, and 20 ns is 40, above tCCD_L_WR's floor of 32.
        let record = ddr5_record(&module_bytes());
        let jedec = column(&record, "JEDEC 4000");
        assert_eq!(["twtr_s", "tccd_l_wr"].map(|k| show(&jedec[k])), ["5", "40"]);
    }

    #[test]
    fn expo_leaves_them_blank() {
        // EXPO stores tRRD_L, tCCD_L, tFAW and tRTP of the floored timings,
        // and none of these three.
        let record = ddr5_record(&module_bytes());
        let expo = record.profiles.iter().find(|p| show(&p["name"]).starts_with("EXPO")).unwrap();
        for key in ["tccd_l_wr", "twtr_l", "twtr_s"] {
            assert_eq!(show(&expo[key]), "—", "{key}");
        }
    }

    fn raw_card(value: u8) -> String {
        let mut values = module_bytes();
        values.insert(232, value);
        decode_ddr5_base(&values).unwrap().unwrap().text("raw_card").unwrap()
    }

    #[test]
    fn the_bench_kit_is_card_a_revision_0() {
        // Read off both of the X870 bench's G.Skill modules: 0x00, which
        // a reference tool also shows as "A Rev 0".
        assert_eq!(raw_card(0x00), "A Rev 0");
    }

    #[test]
    fn the_card_is_the_low_five_bits_and_the_revision_the_top_three() {
        assert_eq!(raw_card(0x01), "B Rev 0");
        assert_eq!(raw_card(0x21), "B Rev 1");
        assert_eq!(raw_card(0xF9), "Z Rev 7");
    }

    #[test]
    fn a_module_off_the_jedec_designs_says_so() {
        assert_eq!(raw_card(0x1F), "None (not a JEDEC design)");
    }

    #[test]
    fn a_code_past_z_is_shown_as_its_number() {
        assert_eq!(raw_card(0x1A), "code 0x1A Rev 0");
    }

    #[test]
    fn decode_failures_are_the_python_exceptions() {
        // An enabled XMP 2.0 profile with no tCK divided by zero, and a DDR5
        // base block listing CAS latencies but no tAA compared None with an
        // int; the SPD page printed both.
        let mut ddr4 = sample_spd();
        ddr4.insert(396, 0);
        ddr4.insert(431, 0);
        assert_eq!(decode_ddr4_xmp(&ddr4), Err("float division by zero".to_string()));
        let mut ddr5 = module_bytes();
        ddr5.insert(30, 0);
        ddr5.insert(31, 0);
        assert!(decode_ddr5_base(&ddr5).unwrap_err().contains("'>' not supported"));
    }
}
