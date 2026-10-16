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

//! Read-only AMD APOB trained-memory data support.
//!
//! This test-only compatibility parser targets an observed Granite Ridge
//! training-result record. It has no write path. Low-level physical access is
//! used only when a live APOB snapshot is requested. Commercial-source
//! provenance approval remains pending; see INTERNAL_TEST_ONLY.txt.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::hardware::{HwError, HwResult, hw};

pub const APOB_SIGNATURE: &[u8; 4] = b"APOB";
pub const GRANITE_RIDGE_BLOCK_SIZE: usize = 0x30;
pub const MAX_APOB_TABLE_SIZE: usize = 4 * 1024 * 1024;
pub const MAX_FIRST_CONFIG_SIZE: usize = 0x1000;
pub const DEFAULT_APOB_ADDRESSES: [u64; 3] = [0x0A20_0000, 0x09F0_0000, 0x0400_0000];

const RTT_OHMS: &[(u8, u32)] = &[(1, 240), (2, 120), (3, 80), (4, 60), (5, 48), (6, 40), (7, 34)];
const GROUP_ODT_OHMS: &[(u8, u32)] = &[(1, 480), (2, 240), (3, 120), (4, 80), (5, 60), (6, 48), (7, 40)];
const PROC_ODT_OHMS: &[(u8, f64)] = &[
    (1, 480.0), (2, 240.0), (3, 160.0), (4, 120.0), (5, 96.0),
    (6, 80.0), (7, 68.6), (12, 60.0), (13, 53.3), (14, 48.0),
    (15, 43.6), (28, 40.0), (29, 36.9), (30, 34.3), (31, 32.0),
    (60, 30.0), (61, 28.2), (62, 26.7), (63, 25.3),
];
const DRAM_DQ_DS_OHMS: &[(u8, u32)] = &[(0, 34), (1, 40), (2, 48)];
const DIRECT_DS_OHMS: &[(u8, Option<u32>)] = &[(0, None), (30, Some(30)), (40, Some(40)), (60, Some(60)), (120, Some(120))];

fn lookup<V: Copy>(table: &[(u8, V)], code: u8) -> Option<V> {
    table.iter().find(|(key, _)| *key == code).map(|(_, value)| *value)
}

// Compatibility labels from the supplied reference 1.0.0 descriptor table.
// These are raw APOB bytes, NOT verified physical VREF/DFE measurements.
// In particular 0x11..0x13 also back the known CA/CK/CS drive strengths below;
// ALERT_PU and CA_DRV deliberately share the reference tool's 0x10 source.
pub const RAW_TRAINING_FIELDS: &[(&str, usize)] = &[
    ("ALERT_PU", 0x10), ("CA_DRV", 0x10),
    ("PHY_VREF", 0x11), ("DQ_VREF", 0x12), ("CA_VREF", 0x13),
    ("CS_VREF", 0x14), ("RX_DFE", 0x15), ("TX_DFE", 0x16),
];

/// One decoded training value: a worded setting, or a raw APOB byte (the
/// [`RAW_TRAINING_FIELDS`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrainingValue {
    Text(String),
    Raw(u8),
}

impl TrainingValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            TrainingValue::Text(text) => Some(text),
            TrainingValue::Raw(_) => None,
        }
    }
}

/// Python's `str(value)`.
impl fmt::Display for TrainingValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TrainingValue::Text(text) => f.write_str(text),
            TrainingValue::Raw(byte) => write!(f, "{byte}"),
        }
    }
}

impl PartialEq<&str> for TrainingValue {
    fn eq(&self, other: &&str) -> bool {
        self.as_str() == Some(*other)
    }
}

impl PartialEq<u8> for TrainingValue {
    fn eq(&self, other: &u8) -> bool {
        *self == TrainingValue::Raw(*other)
    }
}

/// A decoded record: field name -> value.
pub type TrainingValues = BTreeMap<&'static str, TrainingValue>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedApobTraining {
    pub record_offset: usize,
    pub values: TrainingValues,
}

/// Two records attributed to channels by the validated APOB geometry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedApobChannels {
    pub channel_a: ParsedApobTraining,
    pub channel_b: ParsedApobTraining,
}

/// One plausible Granite Ridge record found during a diagnostic scan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GraniteRidgeCandidate {
    pub record_offset: usize,
    pub raw: Vec<u8>,
    pub values: TrainingValues,
}

/// Bounded, read-only evidence about an APOB table's training records.
///
/// This is diagnostic output only: it enumerates every plausible record and
/// the container/main-entry geometry used to find them. It never selects a
/// record, and it holds no physical address.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApobTableDiagnostic {
    pub table_size: usize,
    pub header_size: usize,
    pub sha256: String,
    pub config_offsets: Vec<usize>,
    pub first_offset: usize,
    pub first_size: usize,
    pub main_offset: usize,
    pub main_size: usize,
    pub scan_start: usize,
    pub scan_end: usize,
    pub candidates: Vec<GraniteRidgeCandidate>,
}

fn ohms(value: Option<f64>) -> String {
    let Some(number) = value else {
        return "Off".into();
    };
    let text = if number.fract() == 0.0 { format!("{}", number as i64) } else { format!("{number:.1}") };
    format!("{text} Ω")
}

/// An RTT setting worded the way the Intel backends word it: "40 RZQ/6".
///
/// Resistance first, then the divider, and RZQ itself rather than RZQ/1,
/// so the same setting reads the same on either platform.
fn rtt(code: u8) -> Option<String> {
    if code == 0 {
        return Some("Off".into());
    }
    let ohms = lookup(RTT_OHMS, code)?;
    if code == 1 {
        return Some(format!("{ohms} RZQ"));
    }
    Some(format!("{ohms} RZQ/{code}"))
}

// The divider behind each group-ODT code: RZQ (240 ohms) over 0.5 to 6.
const GROUP_ODT_DIVIDERS: &[(u8, &str)] = &[(1, "0.5"), (2, "1"), (3, "2"), (4, "3"), (5, "4"), (6, "5"), (7, "6")];

/// A CA/CK/CS ODT setting worded as the Intel backends word it.
///
/// Resistance, then the divider: "480 RZQ/0.5", "40 RZQ/6". Intel's DDR5
/// tables write the 240-ohm step "240 RZQ/1" here, unlike the RTT tables'
/// bare "240 RZQ", so this follows that.
fn group_odt(code: u8) -> Option<String> {
    if code == 0 {
        return Some("Off".into());
    }
    let value = lookup(GROUP_ODT_OHMS, code)?;
    Some(format!("{value} RZQ/{}", lookup(GROUP_ODT_DIVIDERS, code)?))
}

fn proc_odt(code: u8) -> Option<String> {
    if code == 0 {
        return Some("Hi-Z".into());
    }
    lookup(PROC_ODT_OHMS, code).map(|value| ohms(Some(value)))
}

fn direct_drive(code: u8) -> Option<String> {
    let value = lookup(DIRECT_DS_OHMS, code)?;
    Some(ohms(value.map(f64::from)))
}

fn dram_drive(code: u8) -> Option<String> {
    lookup(DRAM_DQ_DS_OHMS, code).map(|value| ohms(Some(value as f64)))
}

/// Every field decoded, or `None` if any one of them is not a known code.
fn all_decoded(fields: Vec<(&'static str, Option<String>)>) -> Option<TrainingValues> {
    fields.into_iter().map(|(name, value)| value.map(|text| (name, TrainingValue::Text(text)))).collect()
}

/// Decode one validated 0x30-byte Granite Ridge APOB memory record.
pub fn decode_granite_ridge_training_block(block: &[u8]) -> Option<TrainingValues> {
    let data = block;
    if data.len() < GRANITE_RIDGE_BLOCK_SIZE {
        return None;
    }

    let mut decoded = all_decoded(vec![
        ("rtt_nom_rd", rtt(data[0x1A])),
        ("rtt_nom_wr", rtt(data[0x1B])),
        ("rtt_wr", rtt(data[0x1C])),
        ("rtt_park", rtt(data[0x1D])),
        ("rtt_park_dqs", rtt(data[0x1E])),
        ("ck_odt_a", group_odt(data[0x08])),
        ("cs_odt_a", group_odt(data[0x09])),
        ("ca_odt_a", group_odt(data[0x0A])),
        ("ck_odt_b", group_odt(data[0x0B])),
        ("cs_odt_b", group_odt(data[0x0C])),
        ("ca_odt_b", group_odt(data[0x0D])),
        // The BIOS setting "Processor DQ drive strengths", which drives all DQ
        // and DMI IOs. Confirmed by changing it from Auto to 40 ohm and
        // rebooting: this byte went 30 -> 28, and the only other bytes in the
        // block that moved were the P0 pull up and pull down pair below, which
        // is the same setting reaching the same pins.
        ("proc_dq_ds", proc_odt(data[0x0F])),
        ("proc_ca_ds", direct_drive(data[0x11])),
        ("proc_ck_ds", direct_drive(data[0x12])),
        ("proc_cs_ds", direct_drive(data[0x13])),
        ("dram_dq_ds_pu", dram_drive(data[0x1F])),
        ("dram_dq_ds_pd", dram_drive(data[0x20])),
        ("proc_odt_pu", proc_odt(data[0x21])),
        ("proc_odt_pd", proc_odt(data[0x22])),
        ("proc_dq_ds_pu", proc_odt(data[0x23])),
        ("proc_dq_ds_pd", proc_odt(data[0x24])),
    ])?;
    for (name, offset) in RAW_TRAINING_FIELDS {
        decoded.insert(name, TrainingValue::Raw(data[*offset]));
    }
    Some(decoded)
}

fn u32_at(data: &[u8], offset: usize) -> Result<usize, String> {
    if offset.checked_add(4).is_none_or(|end| end > data.len()) {
        return Err("APOB dword is outside the table".into());
    }
    Ok(u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap()) as usize)
}

/// The bounded first APOB config entry, from table bytes only:
/// `(offset, entry bytes)`.
pub fn extract_first_config_entry(table: &[u8]) -> Result<(usize, Vec<u8>), String> {
    let data = table;
    if data.len() < 0x40 || &data[..4] != APOB_SIGNATURE {
        return Err("APOB signature/header is invalid".into());
    }
    let table_size = u32_at(data, 0x08)?;
    let header_size = u32_at(data, 0x0C)?;
    if !(0x40 <= header_size && header_size <= table_size) {
        return Err("APOB header size is invalid".into());
    }
    if !(header_size <= table_size && table_size <= data.len().min(MAX_APOB_TABLE_SIZE)) {
        return Err("APOB table size is invalid or truncated".into());
    }
    let config_end = header_size - 0x20;
    if config_end <= 0x30 {
        return Err("APOB configuration list is missing".into());
    }
    let mut first = None;
    for offset in (0x30..config_end).step_by(4) {
        let entry = u32_at(data, offset)?;
        if entry != 0 && entry + 0x10 <= table_size {
            first = Some(entry);
            break;
        }
    }
    let Some(first) = first else {
        return Err("APOB contains no valid configuration entries".into());
    };
    let first_size = u32_at(data, first + 0x0C)?;
    if first_size > MAX_FIRST_CONFIG_SIZE {
        return Err("APOB first config exceeds diagnostic limit".into());
    }
    if first_size < 0x10 || first + first_size > table_size {
        return Err("APOB first config is invalid or truncated".into());
    }
    Ok((first, data[first..first + first_size].to_vec()))
}

fn plausible_granite_ridge_block(block: &[u8]) -> bool {
    let data = block;
    if data.len() < GRANITE_RIDGE_BLOCK_SIZE || data[0] == 0 {
        return false;
    }
    if !matches!(data[1], 0 | 1) {
        return false;
    }
    if data[0x08..0x0E].iter().any(|value| *value > 7) {
        return false;
    }
    if data[0x1A..0x1F].iter().any(|value| *value > 7) {
        return false;
    }
    if !data[0x1A..0x1F].iter().any(|value| *value != 0) {
        return false;
    }
    if data[0x11..0x14].iter().any(|value| lookup(DIRECT_DS_OHMS, *value).is_none()) {
        return false;
    }
    if data[0x1F..0x21].iter().any(|value| lookup(DRAM_DQ_DS_OHMS, *value).is_none()) {
        return false;
    }
    if data[0x21..0x25].iter().any(|value| lookup(PROC_ODT_OHMS, *value).is_none() && *value != 0) {
        return false;
    }
    decode_granite_ridge_training_block(data).is_some()
}

/// An APOB table walked to its memory configuration entry, with `data` cut
/// to the declared size.
struct MemoryConfigGeometry<'a> {
    data: &'a [u8],
    table_size: usize,
    header_size: usize,
    config_offsets: Vec<usize>,
    first: usize,
    first_size: usize,
    main: usize,
    main_size: usize,
}

/// Walk an APOB table to its memory configuration entry.
///
/// Shared by the Zen 5 and Zen 4 parsers: the container is the same on both,
/// only the record inside the entry differs.
fn memory_config_geometry(table: &[u8]) -> Result<MemoryConfigGeometry<'_>, String> {
    let data = table;
    if data.len() < 0x40 || &data[..4] != APOB_SIGNATURE {
        return Err("APOB signature/header is invalid".into());
    }
    let table_size = u32_at(data, 0x08)?;
    let header_size = u32_at(data, 0x0C)?;
    if !(0x40 <= header_size && header_size <= table_size) {
        return Err("APOB header size is invalid".into());
    }
    if !(header_size <= table_size && table_size <= data.len().min(MAX_APOB_TABLE_SIZE)) {
        return Err("APOB table size is invalid or truncated".into());
    }
    let data = &data[..table_size];

    let config_end = header_size - 0x20;
    if config_end <= 0x30 {
        return Err("APOB configuration list is missing".into());
    }
    let mut config_offsets = Vec::new();
    for offset in (0x30..config_end).step_by(4) {
        let entry = u32_at(data, offset)?;
        if entry != 0 && entry + 0x10 <= table_size {
            config_offsets.push(entry);
        }
    }
    if config_offsets.is_empty() {
        return Err("APOB contains no valid configuration entries".into());
    }

    let first = config_offsets[0];
    let first_size = u32_at(data, first + 0x0C)?;
    let main = first + first_size;
    if first_size < 0x10 || main + 0x10 > table_size {
        return Err("APOB primary configuration chain is invalid".into());
    }
    if data[main] != 0x01 || data[main + 4] != 0x19 {
        return Err("APOB memory configuration signature is invalid".into());
    }
    let main_size = u32_at(data, main + 0x0C)?;
    Ok(MemoryConfigGeometry { data, table_size, header_size, config_offsets, first, first_size, main, main_size })
}

/// Bounded container metadata and every plausible training record.
///
/// This function accepts table bytes only. It performs no physical access
/// and deliberately does not choose between candidates.
pub fn enumerate_granite_ridge_candidates(table: &[u8]) -> Result<ApobTableDiagnostic, String> {
    let geometry = memory_config_geometry(table)?;
    let data = geometry.data;
    let main_end = geometry.main + geometry.main_size;
    let scan_start = geometry.main + 0x30;
    if geometry.main_size < 0x30 + GRANITE_RIDGE_BLOCK_SIZE || main_end > geometry.table_size {
        return Err("APOB memory configuration block is invalid".into());
    }

    let last_start = main_end - GRANITE_RIDGE_BLOCK_SIZE;
    let mut matches = Vec::new();
    for offset in scan_start..=last_start {
        let block = &data[offset..offset + GRANITE_RIDGE_BLOCK_SIZE];
        if plausible_granite_ridge_block(block) {
            matches.push(GraniteRidgeCandidate {
                record_offset: offset,
                raw: block.to_vec(),
                values: decode_granite_ridge_training_block(block).expect("plausible blocks decode"),
            });
        }
    }

    Ok(ApobTableDiagnostic {
        table_size: geometry.table_size,
        header_size: geometry.header_size,
        sha256: sha256_hex(data),
        config_offsets: geometry.config_offsets,
        first_offset: geometry.first,
        first_size: geometry.first_size,
        main_offset: geometry.main,
        main_size: geometry.main_size,
        scan_start,
        scan_end: main_end,
        candidates: matches,
    })
}

// --- Zen 4 desktop (Ryzen 7000 Raphael), experimental ----------------------
//
// From ZenStates-Core's ApobLayout.cs ("Zen4 19h main"/"extended", as of
// 8979d27), not from a capture on Raphael hardware: nothing decoded here has
// been confirmed by this project, which is why the reader reports it as
// unverified. The block is 0x1A bytes and puts the RTTs at 0x02-0x06 --
// Zen 5 repeats them per P-state at 0x1A-0x1E, and has pull-up/pull-down
// pairs Zen 4 does not, so Zen 4 carries one processor ODT and one DRAM drive
// strength instead. Processor CK and CS drive strength exist only in the
// extended block, found the way ZenStates finds it: by the same five RTT
// bytes.
pub const RAPHAEL_BLOCK_SIZE: usize = 0x1A;
const RAPHAEL_EXTENDED_ENTRY: (u8, u8) = (0x07, 0x03);

/// Decode one 0x1A-byte Zen 4 desktop record, or `None` if any field fails.
pub fn decode_raphael_training_block(block: &[u8], extended: Option<&[u8]>) -> Option<TrainingValues> {
    let data = block;
    if data.len() < RAPHAEL_BLOCK_SIZE {
        return None;
    }
    let mut decoded = all_decoded(vec![
        ("rtt_nom_rd", rtt(data[0x02])),
        ("rtt_nom_wr", rtt(data[0x03])),
        ("rtt_wr", rtt(data[0x04])),
        ("rtt_park", rtt(data[0x05])),
        ("rtt_park_dqs", rtt(data[0x06])),
        ("dram_dq_ds", dram_drive(data[0x07])),
        ("ck_odt_a", group_odt(data[0x08])),
        ("cs_odt_a", group_odt(data[0x09])),
        ("ca_odt_a", group_odt(data[0x0A])),
        ("ck_odt_b", group_odt(data[0x0B])),
        ("cs_odt_b", group_odt(data[0x0C])),
        ("ca_odt_b", group_odt(data[0x0D])),
        ("proc_odt", proc_odt(data[0x0E])),
        ("proc_dq_ds", proc_odt(data[0x0F])),
        ("proc_ca_ds", direct_drive(data[0x11])),
    ])?;
    // The extended pair is extra: an extended block that does not decode
    // leaves those two rows blank rather than discarding the record.
    if let Some(extra) = extended
        && extra.len() >= RAPHAEL_BLOCK_SIZE
        && let (Some(ck), Some(cs)) = (direct_drive(extra[0x12]), direct_drive(extra[0x13]))
    {
        decoded.insert("proc_ck_ds", TrainingValue::Text(ck));
        decoded.insert("proc_cs_ds", TrainingValue::Text(cs));
    }
    Some(decoded)
}

fn plausible_raphael_block(block: &[u8]) -> bool {
    let data = block;
    if data.len() < RAPHAEL_BLOCK_SIZE || data[0] == 0 {
        return false;
    }
    if !matches!(data[1], 0 | 1) {
        return false;
    }
    if data[0x02..0x07].iter().any(|value| *value > 7) || !data[0x02..0x07].iter().any(|value| *value != 0) {
        return false;
    }
    if data[0x08..0x0E].iter().any(|value| *value > 7) {
        return false;
    }
    decode_raphael_training_block(data, None).is_some()
}

/// The Zen 4 desktop record and where it sits.
///
/// The record starts at the first non-zero byte past the memory entry's
/// 0x30-byte lead, as ZenStates reads it. Where ZenStates would decode
/// whatever sits there, this also requires the block to be plausible, so an
/// APOB laid out some other way yields nothing rather than a wrong setting.
pub fn parse_raphael_apob_table(table: &[u8]) -> Result<ParsedApobTraining, String> {
    let geometry = memory_config_geometry(table)?;
    let data = geometry.data;
    let table_size = geometry.table_size;
    let main_end = geometry.main + geometry.main_size;
    if geometry.main_size < 0x30 + RAPHAEL_BLOCK_SIZE || main_end > table_size {
        return Err("APOB memory configuration block is invalid".into());
    }
    let start = (geometry.main + 0x30..main_end).find(|offset| data[*offset] != 0);
    let Some(start) = start.filter(|start| start + RAPHAEL_BLOCK_SIZE <= main_end) else {
        return Err("No Ryzen 7000 training record found".into());
    };
    let block = &data[start..start + RAPHAEL_BLOCK_SIZE];
    if !plausible_raphael_block(block) {
        return Err("No plausible Ryzen 7000 training record found".into());
    }

    let mut extended = None;
    let rtts = &block[0x02..0x07];
    for offset in &geometry.config_offsets {
        let offset = *offset;
        if offset + 0x10 > table_size {
            continue;
        }
        if (data[offset], data[offset + 4]) != RAPHAEL_EXTENDED_ENTRY {
            continue;
        }
        let size = u32_at(data, offset + 0x0C)?;
        let entry = &data[offset..(offset + size).min(table_size)];
        if size < RAPHAEL_BLOCK_SIZE {
            continue;
        }
        if let Some(found) = entry.windows(rtts.len()).position(|window| window == rtts)
            && found >= 2
            && found - 2 + RAPHAEL_BLOCK_SIZE <= entry.len()
        {
            extended = Some(&entry[found - 2..found - 2 + RAPHAEL_BLOCK_SIZE]);
        }
        break;
    }

    let values = decode_raphael_training_block(block, extended)
        .ok_or_else(|| "No plausible Ryzen 7000 training record found".to_string())?;
    Ok(ParsedApobTraining { record_offset: start, values })
}

pub const CCDL_WR_MAX_RATIO: u32 = 4;

// The pair that marks a per-channel frequency record, and how far past it the
// [tCCD_L, tCCD_L_WR, tCCD_L_WR2] run sits, in 16-bit elements. Taken from
// ZenTimings, which moved all three readings out of the UMC registers and on
// to this marker because some boards never program 0x50198 from the BIOS
// setting at all -- so a register that disagrees with the BIOS is not a
// misread, it is a register the firmware never wrote.
//
// The marker is the point: a BIOS cannot change it, so the run is found
// without trusting the registers to be right. On this bench it appears twice,
// once per channel, and both runs read (21, 83, 42) -- the same values the
// registers give here, which is what says adopting it changes nothing where
// the registers are already correct.
pub const CCDL_RUN_MARKER: (u16, u16) = (0x5000, 0x00C3);
pub const CCDL_RUN_OFFSET: usize = 9;

// Plausible ranges, the same ones ZenTimings applies. The encoding was
// derived empirically and may not hold on every AGESA, so a value outside
// these is dropped rather than displayed.
pub const CCDL_RANGE: (u32, u32) = (8, 36);
pub const CCDL_WR2_RANGE: (u32, u32) = (8, 70);

fn u16_at(data: &[u8], offset: usize) -> u32 {
    u16::from_le_bytes([data[offset], data[offset + 1]]) as u32
}

/// `(tCCD_L, tCCD_L_WR, tCCD_L_WR2)` from the APOB, or `None`.
///
/// Located by the record marker rather than by using the registers as an
/// anchor, so it still answers on a board whose registers do not follow the
/// BIOS. Every marker in the table must yield the same run: the pair appears
/// once per channel, and channels that disagree mean the marker is being
/// matched somewhere it does not belong.
pub fn find_ccdl_run(table: &[u8]) -> Option<(u32, u32, u32)> {
    if table.len() < 6 {
        return None;
    }

    let mut found = BTreeSet::new();
    let mut marker = [0u8; 4];
    marker[..2].copy_from_slice(&CCDL_RUN_MARKER.0.to_le_bytes());
    marker[2..].copy_from_slice(&CCDL_RUN_MARKER.1.to_le_bytes());
    let limit = table.len() - marker.len();
    for offset in (0..=limit).step_by(2) {
        if table[offset..offset + marker.len()] != marker {
            continue;
        }
        let start = offset + CCDL_RUN_OFFSET * 2;
        if start + 6 > table.len() {
            continue;
        }
        found.insert((u16_at(table, start), u16_at(table, start + 2), u16_at(table, start + 4)));
    }

    if found.len() != 1 {
        return None;
    }
    let (tccdl, tccdl_wr, tccdl_wr2) = found.pop_first()?;
    if !(CCDL_RANGE.0 <= tccdl && tccdl <= CCDL_RANGE.1) {
        return None;
    }
    if !(CCDL_WR2_RANGE.0 <= tccdl_wr2 && tccdl_wr2 <= CCDL_WR2_RANGE.1) {
        return None;
    }
    if !(tccdl <= tccdl_wr && tccdl_wr <= CCDL_WR_MAX_RATIO * tccdl) {
        return None;
    }
    Some((tccdl, tccdl_wr, tccdl_wr2))
}

/// tCCD_L_WR from the APOB, or `None` when it cannot be pinned down.
///
/// tCCD_L_WR is the one of the three tCCD_L timings that no UMC register
/// holds. The APOB carries all three next to each other as 16-bit values, in
/// the order tCCD_L, tCCD_L_WR, tCCD_L_WR2, so the two values that *were*
/// read from registers act as an anchor: find the places where those two
/// bracket a third, and the value between them is tCCD_L_WR.
///
/// That is deliberately stricter than a bare pattern scan. Every match must
/// agree, and the result must satisfy tCCD_L <= tCCD_L_WR <= 4 * tCCD_L --
/// the same bound ZenTimings applies -- so a coincidental byte pattern
/// elsewhere in the table produces `None` rather than a wrong number.
pub fn find_ccdl_wr(table: &[u8], tccdl: Option<u32>, tccdl_wr2: Option<u32>) -> Option<u32> {
    let (Some(tccdl), Some(tccdl_wr2)) = (tccdl.filter(|v| *v != 0), tccdl_wr2.filter(|v| *v != 0)) else {
        return None;
    };
    if table.is_empty() || table.len() < 6 {
        return None;
    }

    let mut found = BTreeSet::new();
    let limit = table.len() - 6;
    for offset in (0..=limit).step_by(2) {
        let (first, middle, last) = (u16_at(table, offset), u16_at(table, offset + 2), u16_at(table, offset + 4));
        if first == tccdl && last == tccdl_wr2 {
            found.insert(middle);
        }
    }

    if found.len() != 1 {
        return None;
    }
    let value = found.pop_first()?;
    if !(tccdl <= value && value <= CCDL_WR_MAX_RATIO * tccdl) {
        return None;
    }
    Some(value)
}

/// Locate one unambiguous trained-memory record in a bounded APOB table.
pub fn parse_apob_table(table: &[u8]) -> Result<ParsedApobTraining, String> {
    let diagnostic = enumerate_granite_ridge_candidates(table)?;
    if diagnostic.candidates.len() == 1 {
        let candidate = &diagnostic.candidates[0];
        return Ok(ParsedApobTraining { record_offset: candidate.record_offset, values: candidate.values.clone() });
    }
    if diagnostic.candidates.len() > 1 {
        let first = &diagnostic.candidates[0];
        let consensus = diagnostic.candidates.iter().enumerate().all(|(index, candidate)| {
            candidate.raw == first.raw
                && candidate.record_offset == first.record_offset + index * GRANITE_RIDGE_BLOCK_SIZE
        });
        if consensus {
            return Ok(ParsedApobTraining { record_offset: first.record_offset, values: first.values.clone() });
        }
        return Err("APOB contains multiple plausible Granite Ridge training records".into());
    }
    Err("No plausible Granite Ridge training record found".into())
}

/// Attribute ChA/ChB only for the exact validated two-record memory block.
///
/// The observed B850MPOWER layout has a 0x30-byte lead followed by exactly
/// two non-overlapping 0x30-byte records. Candidate enumeration scans every
/// byte, so requiring exactly the two boundary offsets also rejects hidden
/// overlapping or misaligned candidates. The record payload has no decoded
/// channel identifier, so lower=ChA and upper=ChB remains internal hardware-
/// validation attribution. Other layouts remain unavailable.
pub fn parse_apob_channel_records(table: &[u8]) -> Result<ParsedApobChannels, String> {
    channel_records(enumerate_granite_ridge_candidates(table)?)
}

fn channel_records(diagnostic: ApobTableDiagnostic) -> Result<ParsedApobChannels, String> {
    let expected_offsets = [diagnostic.scan_start, diagnostic.scan_start + GRANITE_RIDGE_BLOCK_SIZE];
    let actual_offsets: Vec<usize> = diagnostic.candidates.iter().map(|candidate| candidate.record_offset).collect();
    if diagnostic.main_size != 0x90
        || diagnostic.scan_end - diagnostic.scan_start != 2 * GRANITE_RIDGE_BLOCK_SIZE
        || diagnostic.candidates.len() != 2
        || actual_offsets != expected_offsets
    {
        return Err("APOB channel geometry is not the validated two-record layout".into());
    }

    let mut candidates = diagnostic.candidates.into_iter();
    let (channel_a, channel_b) = (candidates.next().unwrap(), candidates.next().unwrap());
    Ok(ParsedApobChannels {
        channel_a: ParsedApobTraining { record_offset: channel_a.record_offset, values: channel_a.values },
        channel_b: ParsedApobTraining { record_offset: channel_b.record_offset, values: channel_b.values },
    })
}

/// Which record layout to decode: Ryzen 9000's validated one, or the
/// experimental Ryzen 7000 one.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ApobLayout {
    #[default]
    Zen5,
    Zen4,
}

impl ApobLayout {
    /// The Python name of the layout.
    pub fn name(self) -> &'static str {
        match self {
            ApobLayout::Zen5 => "zen5",
            ApobLayout::Zen4 => "zen4",
        }
    }

    /// The layout called `name`, refusing anything else as the Python
    /// constructor did.
    pub fn from_name(name: &str) -> Result<Self, String> {
        match name {
            "zen5" => Ok(ApobLayout::Zen5),
            "zen4" => Ok(ApobLayout::Zen4),
            other => Err(format!("unknown APOB layout '{other}'")),
        }
    }
}

/// A 32-bit physical read: the injected `read_dword` of the Python reader.
pub type ReadDword = Box<dyn Fn(u64) -> HwResult<u32> + Send + Sync>;

/// One table that parsed: `(address, table, parsed, raw record, channels)`.
type ApobSuccess = (u64, Vec<u8>, ParsedApobTraining, Vec<u8>, Option<ParsedApobChannels>);

/// Locate and decode a bounded APOB table through read-only physical I/O.
pub struct GraniteRidgeApobReader {
    pub layout: ApobLayout,
    read_dword: Option<ReadDword>,
    candidate_addresses: Vec<u64>,
    pub table_address: Option<u64>,
    pub record_address: Option<u64>,
    pub raw_table: Vec<u8>,
    pub raw_record: Vec<u8>,
    pub channel_values: BTreeMap<&'static str, TrainingValues>,
    pub channel_record_addresses: BTreeMap<&'static str, u64>,
    pub channel_raw_records: BTreeMap<&'static str, Vec<u8>>,
    pub ambiguous_candidates: Vec<(u64, ApobTableDiagnostic)>,
    pub last_error: String,
}

impl GraniteRidgeApobReader {
    /// The live reader over [`DEFAULT_APOB_ADDRESSES`], reading through the
    /// driver's `GetPhysLong`.
    pub fn new(layout: ApobLayout) -> Self {
        Self::build(None, DEFAULT_APOB_ADDRESSES.to_vec(), layout)
    }

    /// A reader on an explicit `read_dword` and candidate list.
    pub fn with_read_dword(
        read_dword: impl Fn(u64) -> HwResult<u32> + Send + Sync + 'static,
        candidate_addresses: &[u64],
        layout: ApobLayout,
    ) -> Self {
        Self::build(Some(Box::new(read_dword)), candidate_addresses.to_vec(), layout)
    }

    fn build(read_dword: Option<ReadDword>, candidate_addresses: Vec<u64>, layout: ApobLayout) -> Self {
        GraniteRidgeApobReader {
            layout,
            read_dword,
            candidate_addresses,
            table_address: None,
            record_address: None,
            raw_table: Vec::new(),
            raw_record: Vec::new(),
            channel_values: BTreeMap::new(),
            channel_record_addresses: BTreeMap::new(),
            channel_raw_records: BTreeMap::new(),
            ambiguous_candidates: Vec::new(),
            last_error: String::new(),
        }
    }

    fn resolve_reader(&mut self) -> Result<&ReadDword, String> {
        if self.read_dword.is_none() {
            // The Python resolved InpOut here: a missing DLL raised its
            // message, a loaded driver that is not open raised this one.
            if let Some(error) = crate::hardware::driver_error() {
                return Err(error);
            }
            if !hw().is_driver_open() {
                return Err("InpOut driver is not open".into());
            }
            self.read_dword = Some(Box::new(|address| hw().get_phys_long(address)));
        }
        Ok(self.read_dword.as_ref().expect("resolved above"))
    }

    fn read_bytes(read_dword: &ReadDword, address: u64, size: usize) -> HwResult<Vec<u8>> {
        if size > MAX_APOB_TABLE_SIZE {
            return Err(HwError::new("physical read size is outside the APOB limit"));
        }
        let start = address & !3;
        let end = (address + size as u64 + 3) & !3;
        let mut data = Vec::with_capacity((end - start) as usize);
        let mut current = start;
        while current < end {
            data.extend_from_slice(&read_dword(current)?.to_le_bytes());
            current += 4;
        }
        let offset = (address - start) as usize;
        Ok(data[offset..offset + size].to_vec())
    }

    /// Read one candidate base: `Ok(None)` when no APOB signature is there.
    fn read_candidate(
        &self,
        read_dword: &ReadDword,
        address: u64,
        diagnostic: &mut Option<ApobTableDiagnostic>,
    ) -> Result<Option<ApobSuccess>, String> {
        let header = Self::read_bytes(read_dword, address, 0x10).map_err(|e| e.0)?;
        if &header[..4] != APOB_SIGNATURE {
            return Ok(None);
        }
        let table_size = u32_at(&header, 0x08)?;
        if !(0x40..=MAX_APOB_TABLE_SIZE).contains(&table_size) {
            return Err("declared table size is outside the APOB limit".into());
        }
        let table = Self::read_bytes(read_dword, address, table_size).map_err(|e| e.0)?;
        let header_after = Self::read_bytes(read_dword, address, 0x10).map_err(|e| e.0)?;
        if header_after != header {
            return Err("APOB header changed during the body read".into());
        }
        let mut channels = None;
        let (parsed, block_size) = if self.layout == ApobLayout::Zen4 {
            // One record, no channel attribution: the two-record geometry
            // that attributes channels is Zen 5's.
            (parse_raphael_apob_table(&table)?, RAPHAEL_BLOCK_SIZE)
        } else {
            *diagnostic = Some(enumerate_granite_ridge_candidates(&table)?);
            let parsed = match parse_apob_channel_records(&table) {
                Ok(found) => {
                    let channel_a = found.channel_a.clone();
                    channels = Some(found);
                    channel_a
                }
                Err(_) => parse_apob_table(&table)?,
            };
            (parsed, GRANITE_RIDGE_BLOCK_SIZE)
        };
        let raw_record = table[parsed.record_offset..parsed.record_offset + block_size].to_vec();
        Ok(Some((address, table, parsed, raw_record, channels)))
    }

    /// Find, read and decode the table. Returns the decoded record, or
    /// `None` with [`GraniteRidgeApobReader::last_error`] saying why.
    pub fn read(&mut self) -> Option<TrainingValues> {
        self.last_error.clear();
        self.table_address = None;
        self.record_address = None;
        self.raw_table.clear();
        self.raw_record.clear();
        self.channel_values.clear();
        self.channel_record_addresses.clear();
        self.channel_raw_records.clear();
        self.ambiguous_candidates.clear();
        let mut errors = Vec::new();
        let mut ambiguous = Vec::new();
        if let Err(error) = self.resolve_reader() {
            self.last_error = format!("APOB physical reader unavailable: {error}");
            return None;
        }
        let read_dword = self.read_dword.take().expect("resolved");

        let mut successes = Vec::new();
        for address in self.candidate_addresses.clone() {
            let mut diagnostic = None;
            match self.read_candidate(&read_dword, address, &mut diagnostic) {
                Ok(Some(success)) => successes.push(success),
                Ok(None) => {}
                Err(error) => {
                    if let Some(diagnostic) = diagnostic
                        && diagnostic.candidates.len() > 1
                    {
                        ambiguous.push((address, diagnostic));
                    }
                    errors.push(format!("0x{address:08X}: {error}"));
                }
            }
        }
        self.read_dword = Some(read_dword);
        self.ambiguous_candidates = ambiguous;
        if successes.len() == 1 {
            let (address, table, parsed, raw_record, channels) = successes.pop().expect("one success");
            self.table_address = Some(address);
            self.record_address = Some(address + parsed.record_offset as u64);
            if let Some(channels) = channels {
                for (name, item) in [("cha", &channels.channel_a), ("chb", &channels.channel_b)] {
                    self.channel_values.insert(name, item.values.clone());
                    self.channel_record_addresses.insert(name, address + item.record_offset as u64);
                    self.channel_raw_records.insert(
                        name,
                        table[item.record_offset..item.record_offset + GRANITE_RIDGE_BLOCK_SIZE].to_vec(),
                    );
                }
            }
            self.raw_table = table;
            self.raw_record = raw_record;
            return Some(parsed.values);
        }
        if successes.len() > 1 {
            let bases: Vec<String> = successes.iter().map(|item| format!("0x{:08X}", item.0)).collect();
            self.last_error =
                format!("APOB training data unavailable (multiple valid candidate bases: {})", bases.join(", "));
            return None;
        }
        let detail = if errors.is_empty() { "signature not found".to_string() } else { errors.join("; ") };
        self.last_error = format!("APOB training data unavailable ({detail})");
        None
    }
}

/// SHA-256 of `data` as lowercase hex: Python's
/// `hashlib.sha256(data).hexdigest()`, for the diagnostic fingerprint.
fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
        0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
        0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
        0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
        0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
        0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
        0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut message = data.to_vec();
    let bit_length = (data.len() as u64).wrapping_mul(8);
    message.push(0x80);
    while message.len() % 64 != 56 {
        message.push(0);
    }
    message.extend_from_slice(&bit_length.to_be_bytes());
    for chunk in message.as_chunks::<64>().0 {
        let mut w = [0u32; 64];
        for (i, word) in chunk.as_chunks::<4>().0.iter().enumerate() {
            w[i] = u32::from_be_bytes(*word);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (slot, value) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *slot = slot.wrapping_add(value);
        }
    }
    h.iter().map(|word| format!("{word:08x}")).collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn put_u32(table: &mut [u8], offset: usize, value: u32) {
        table[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn training_block() -> Vec<u8> {
        let mut block = vec![0u8; 0x30];
        block[0x00] = 1;
        block[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
        block[0x11..0x14].copy_from_slice(&[40, 60, 30]);
        block[0x1A..0x1F].copy_from_slice(&[0, 0, 6, 5, 6]);
        block[0x1F..0x21].copy_from_slice(&[0, 1]);
        block[0x21..0x25].copy_from_slice(&[28, 30, 28, 30]);
        block
    }

    pub(crate) fn apob_table(block: Option<&[u8]>) -> Vec<u8> {
        let default = training_block();
        let block = block.unwrap_or(&default);
        let mut table = vec![0u8; 0x140];
        table[0..4].copy_from_slice(b"APOB");
        let length = table.len() as u32;
        put_u32(&mut table, 4, 1);
        put_u32(&mut table, 8, length);
        put_u32(&mut table, 12, 0x60);
        let first_offset = 0x80;
        put_u32(&mut table, 0x30, first_offset as u32);
        put_u32(&mut table, first_offset + 0x0C, 0x20);
        let main_offset = first_offset + 0x20;
        table[main_offset] = 0x01;
        table[main_offset + 4] = 0x19;
        put_u32(&mut table, main_offset + 0x0C, 0x80);
        table[main_offset + 0x30..main_offset + 0x60].copy_from_slice(block);
        table
    }

    /// An APOB table whose memory-config block holds two plausible records.
    fn ambiguous_apob_table() -> Vec<u8> {
        let mut table = apob_table(None);
        let main_offset = 0xA0;
        put_u32(&mut table, main_offset + 0x0C, 0xA0);
        table[0x110..0x140].copy_from_slice(&training_block());
        table
    }

    fn from_hex(text: &str) -> Vec<u8> {
        (0..text.len()).step_by(2).map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap()).collect()
    }

    /// Exact byte oracle from the 2026-08-06 B850MPOWER capture.
    fn real_training_block() -> Vec<u8> {
        from_hex(concat!(
            "01010000060506000000010505050C1E",
            "0C1E1E1E5D5834340100000006050602",
            "001E001E1E0000000000000000000000",
        ))
    }

    fn duplicate_real_apob_table(second: Option<&[u8]>, gap: usize) -> Vec<u8> {
        let first = real_training_block();
        let second = second.map(<[u8]>::to_vec).unwrap_or_else(|| first.clone());
        let mut table = apob_table(Some(&first));
        let main_offset = 0xA0;
        let second_offset = 0xD0 + first.len() + gap;
        let main_end = second_offset + second.len();
        if main_end > table.len() {
            table.resize(main_end, 0);
        }
        let length = table.len() as u32;
        put_u32(&mut table, 0x08, length);
        put_u32(&mut table, main_offset + 0x0C, (main_end - main_offset) as u32);
        table[second_offset..second_offset + second.len()].copy_from_slice(&second);
        table
    }

    /// Exact observed geometry with deliberately distinct valid channel data.
    fn distinct_channel_apob_table() -> Vec<u8> {
        let mut channel_b = real_training_block();
        channel_b[0x0A] = 2; // Group-A CA ODT: 240 ohms
        channel_b[0x0D] = 6; // Group-B CA ODT: 48 ohms
        channel_b[0x1C] = 4; // RTT WR: RZQ/4 (60 ohms)
        channel_b[0x21] = 12; // Processor ODT pull-up: 60 ohms
        duplicate_real_apob_table(Some(&channel_b), 0)
    }

    fn sha(data: &[u8]) -> String {
        sha256_hex(data)
    }

    #[test]
    fn sha256_matches_known_digests() {
        assert_eq!(sha(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(
            sha(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    // -- ProcDqDriveStrengthTest -----------------------------------------
    //
    // 0x0F was confirmed by changing the BIOS setting, not by resemblance.
    //
    // Processor DQ drive strengths went from Auto to 40 ohm across a reboot
    // and this byte followed, 30 -> 28. Three bytes in the whole block moved:
    // this one and the P0 pull up and pull down pair, which is the same
    // setting reaching the same pins. Nothing else stirred, so it is not a
    // counter.

    /// Decode just the DQ drive strength from an otherwise valid block.
    fn proc_odt_of(dq: u8) -> TrainingValue {
        let mut block = vec![0u8; 0x30];
        block[0x0F] = dq;
        block[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
        block[0x11..0x14].copy_from_slice(&[40, 60, 30]);
        block[0x1A..0x1F].copy_from_slice(&[0, 0, 6, 5, 6]);
        block[0x1F..0x21].copy_from_slice(&[0, 1]);
        block[0x21..0x25].copy_from_slice(&[28, 30, 28, 30]);
        decode_granite_ridge_training_block(&block).unwrap()["proc_dq_ds"].clone()
    }

    #[test]
    fn the_auto_reading_decodes() {
        assert_eq!(proc_odt_of(30), "34.3 Ω");
    }

    #[test]
    fn the_setting_that_was_dialled_in_decodes() {
        assert_eq!(proc_odt_of(28), "40 Ω");
    }

    #[test]
    fn high_impedance_is_an_option_on_this_board() {
        // The board's dropdown offers it beside the impedances, so zero here
        // is a choice rather than an unread byte.
        assert_eq!(proc_odt_of(0), "Hi-Z");
    }

    // -- GraniteRidgeTrainingDecodeTest ----------------------------------

    #[test]
    fn decodes_rtt_group_odt_and_drive_strengths() {
        let mut block = vec![0u8; 0x30];
        block[0x01] = 0; // GDM
        block[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
        block[0x0F] = 28; // Processor DQ drive strengths
        block[0x11..0x14].copy_from_slice(&[40, 60, 30]);
        block[0x1A..0x1F].copy_from_slice(&[0, 0, 6, 5, 6]);
        block[0x1F..0x21].copy_from_slice(&[0, 1]);
        block[0x21..0x25].copy_from_slice(&[28, 30, 28, 30]);

        let decoded = decode_granite_ridge_training_block(&block).unwrap();

        let expected = [
            ("rtt_nom_wr", "Off"),
            ("rtt_nom_rd", "Off"),
            ("rtt_wr", "40 RZQ/6"),
            ("rtt_park", "48 RZQ/5"),
            ("rtt_park_dqs", "40 RZQ/6"),
            ("ca_odt_a", "480 RZQ/0.5"),
            ("ck_odt_a", "Off"),
            ("cs_odt_a", "Off"),
            ("ca_odt_b", "60 RZQ/4"),
            ("ck_odt_b", "60 RZQ/4"),
            ("cs_odt_b", "60 RZQ/4"),
            ("proc_odt_pu", "40 Ω"),
            ("proc_odt_pd", "34.3 Ω"),
            ("proc_ca_ds", "40 Ω"),
            ("proc_ck_ds", "60 Ω"),
            ("proc_cs_ds", "30 Ω"),
            ("proc_dq_ds", "40 Ω"),
            ("proc_dq_ds_pu", "40 Ω"),
            ("proc_dq_ds_pd", "34.3 Ω"),
            ("dram_dq_ds_pu", "34 Ω"),
            ("dram_dq_ds_pd", "40 Ω"),
        ];
        for (name, text) in expected {
            assert_eq!(decoded[name], text, "{name}");
        }
    }

    // -- ApobTableParseTest ----------------------------------------------

    #[test]
    fn finds_valid_training_record_inside_apob_container() {
        let parsed = parse_apob_table(&apob_table(None)).unwrap();
        assert_eq!(parsed.record_offset, 0xD0);
        assert_eq!(parsed.values["rtt_wr"], "40 RZQ/6");
        assert_eq!(parsed.values["ca_odt_b"], "60 RZQ/4");
    }

    #[test]
    fn rejects_ambiguous_multiple_training_records() {
        let mut table = apob_table(None);
        let main_offset = 0xA0;
        put_u32(&mut table, main_offset + 0x0C, 0xA0);
        table[0x110..0x140].copy_from_slice(&training_block());
        assert!(parse_apob_table(&table).unwrap_err().contains("multiple"));
    }

    #[test]
    fn accepts_contiguous_byte_identical_duplicate_records() {
        let table = duplicate_real_apob_table(None, 0);

        let parsed = parse_apob_table(&table).unwrap();

        assert_eq!(parsed.record_offset, 0xD0);
        assert_eq!(parsed.values["rtt_wr"], "40 RZQ/6");
    }

    #[test]
    fn rejects_same_decoding_when_an_ignored_raw_byte_differs() {
        let mut second = real_training_block();
        second[0x25] = 1;
        let table = duplicate_real_apob_table(Some(&second), 0);
        let candidates = enumerate_granite_ridge_candidates(&table).unwrap().candidates;
        assert_eq!(candidates[0].values, candidates[1].values);

        assert!(parse_apob_table(&table).unwrap_err().contains("multiple"));
    }

    #[test]
    fn rejects_identical_records_when_not_contiguous() {
        let table = duplicate_real_apob_table(None, 0x10);

        assert!(parse_apob_table(&table).unwrap_err().contains("multiple"));
    }

    // -- ApobChannelParseTest --------------------------------------------

    #[test]
    fn maps_exact_two_record_geometry_to_distinct_channels() {
        let parsed = parse_apob_channel_records(&distinct_channel_apob_table()).unwrap();

        assert_eq!(parsed.channel_a.record_offset, 0xD0);
        assert_eq!(parsed.channel_b.record_offset, 0x100);
        assert_eq!(parsed.channel_a.values["rtt_wr"], "40 RZQ/6");
        assert_eq!(parsed.channel_b.values["rtt_wr"], "60 RZQ/4");
        assert_eq!(parsed.channel_a.values["ca_odt_a"], "480 RZQ/0.5");
        assert_eq!(parsed.channel_b.values["ca_odt_a"], "240 RZQ/1");
        assert_eq!(parsed.channel_a.values["ca_odt_b"], "60 RZQ/4");
        assert_eq!(parsed.channel_b.values["ca_odt_b"], "48 RZQ/5");
    }

    #[test]
    fn maps_byte_identical_boundary_records_as_two_channels() {
        let parsed = parse_apob_channel_records(&duplicate_real_apob_table(None, 0)).unwrap();

        assert_eq!(parsed.channel_a.record_offset, 0xD0);
        assert_eq!(parsed.channel_b.record_offset, 0x100);
        assert_eq!(parsed.channel_a.values, parsed.channel_b.values);
    }

    #[test]
    fn rejects_wrong_main_geometry_even_with_two_valid_records() {
        let error = parse_apob_channel_records(&duplicate_real_apob_table(None, 0x10)).unwrap_err();
        assert!(error.contains("channel geometry"));
    }

    #[test]
    fn rejects_exact_size_when_only_one_boundary_record_is_valid() {
        let table = duplicate_real_apob_table(Some(&[0u8; 0x30]), 0);

        assert!(parse_apob_channel_records(&table).unwrap_err().contains("channel geometry"));
    }

    #[test]
    fn rejects_extra_or_misaligned_candidate() {
        // The Python patched enumerate_granite_ridge_candidates to return the
        // polluted diagnostic; here it is handed to the same check directly.
        let table = duplicate_real_apob_table(None, 0);
        let diagnostic = enumerate_granite_ridge_candidates(&table).unwrap();
        let extra = GraniteRidgeCandidate {
            record_offset: diagnostic.scan_start + 1,
            raw: diagnostic.candidates[0].raw.clone(),
            values: diagnostic.candidates[0].values.clone(),
        };
        let mut polluted = diagnostic.clone();
        polluted.candidates.push(extra);
        assert!(channel_records(polluted).unwrap_err().contains("channel geometry"));
        // and unpolluted, the same diagnostic is accepted
        assert!(channel_records(diagnostic).is_ok());
    }

    #[test]
    fn channel_parser_accepts_only_table_bytes() {
        // Python checked the signature is exactly (table); the type says so.
        let parser: fn(&[u8]) -> Result<ParsedApobChannels, String> = parse_apob_channel_records;
        assert!(parser(&[]).is_err());
    }

    // -- EnumerateGraniteRidgeCandidatesTest -----------------------------

    #[test]
    fn extracts_only_bounded_first_general_config_entry() {
        let table = apob_table(None);

        let (offset, raw) = extract_first_config_entry(&table).unwrap();

        assert_eq!(offset, 0x80);
        assert_eq!(raw, table[0x80..0xA0].to_vec());
    }

    #[test]
    fn first_config_extraction_rejects_oversized_entry() {
        let mut table = apob_table(None);
        put_u32(&mut table, 0x80 + 0x0C, 0x1001);
        assert!(extract_first_config_entry(&table).unwrap_err().contains("limit"));
    }

    #[test]
    fn single_record_table_reports_one_candidate_with_metadata() {
        let table = apob_table(None);

        let diag = enumerate_granite_ridge_candidates(&table).unwrap();

        assert_eq!(diag.table_size, table.len());
        assert_eq!(diag.header_size, 0x60);
        assert_eq!(diag.config_offsets, vec![0x80]);
        assert_eq!(diag.first_offset, 0x80);
        assert_eq!(diag.first_size, 0x20);
        assert_eq!(diag.main_offset, 0xA0);
        assert_eq!(diag.main_size, 0x80);
        assert_eq!(diag.scan_start, 0xD0);
        assert_eq!(diag.sha256, sha(&table));
        assert_eq!(diag.candidates.len(), 1);
        let candidate = &diag.candidates[0];
        assert_eq!(candidate.record_offset, 0xD0);
        assert_eq!(candidate.raw.len(), 0x30);
        assert_eq!(candidate.raw, table[0xD0..0x100].to_vec());
        assert_eq!(candidate.values["rtt_wr"], "40 RZQ/6");
    }

    #[test]
    fn enumerates_every_plausible_candidate_without_selecting() {
        let table = ambiguous_apob_table();

        let diag = enumerate_granite_ridge_candidates(&table).unwrap();

        assert_eq!(diag.candidates.iter().map(|c| c.record_offset).collect::<Vec<_>>(), vec![0xD0, 0xF4, 0x110]);
        let parks: Vec<String> = diag.candidates.iter().map(|c| c.values["rtt_park"].to_string()).collect();
        assert_eq!(parks, vec!["48 RZQ/5", "Off", "48 RZQ/5"]);
        for candidate in &diag.candidates {
            assert_eq!(candidate.raw.len(), 0x30);
        }
        assert_eq!(diag.main_offset, 0xA0);
        assert_eq!(diag.main_size, 0xA0);
        assert_eq!(diag.sha256, sha(&table));
        // Enumeration is evidence only: the parser must still refuse to pick one.
        assert!(parse_apob_table(&table).unwrap_err().contains("multiple"));
    }

    #[test]
    fn enumerate_accepts_only_a_table_no_address_or_write_input() {
        // Python checked the signature is exactly (table); the type says so.
        let enumerate: fn(&[u8]) -> Result<ApobTableDiagnostic, String> = enumerate_granite_ridge_candidates;
        assert!(enumerate(&[]).is_err());
    }

    #[test]
    fn enumerate_rejects_invalid_header() {
        assert!(enumerate_granite_ridge_candidates(&[0u8; 0x80]).is_err());
    }

    // -- ApobPhysicalReaderTest ------------------------------------------

    fn dwords_at(base: u64, table: &[u8]) -> BTreeMap<u64, u32> {
        (0..table.len())
            .step_by(4)
            .map(|offset| {
                let mut word = [0u8; 4];
                let end = (offset + 4).min(table.len());
                word[..end - offset].copy_from_slice(&table[offset..end]);
                (base + offset as u64, u32::from_le_bytes(word))
            })
            .collect()
    }

    fn reader_over(dwords: BTreeMap<u64, u32>, candidates: &[u64]) -> GraniteRidgeApobReader {
        GraniteRidgeApobReader::with_read_dword(
            move |address| Ok(dwords.get(&address).copied().unwrap_or(0)),
            candidates,
            ApobLayout::Zen5,
        )
    }

    #[test]
    fn skips_invalid_candidate_and_reads_bounded_valid_table() {
        let table = apob_table(None);
        let valid_base = 0x2000;
        let dwords = dwords_at(valid_base, &table);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();

        let mut reader = GraniteRidgeApobReader::with_read_dword(
            move |address| {
                seen.lock().unwrap().push(address);
                Ok(dwords.get(&address).copied().unwrap_or(0))
            },
            &[0x1000, valid_base],
            ApobLayout::Zen5,
        );
        let values = reader.read().unwrap();

        assert_eq!(values["rtt_park"], "48 RZQ/5");
        assert_eq!(reader.table_address, Some(valid_base));
        assert_eq!(reader.record_address, Some(valid_base + 0xD0));
        let calls = calls.lock().unwrap();
        assert!(calls.contains(&0x1000));
        assert!(*calls.iter().max().unwrap() <= valid_base + table.len() as u64 - 4);
    }

    #[test]
    fn oversized_table_is_rejected_before_body_read() {
        let base = 0x3000;
        let mut header = vec![0u8; 0x10];
        header[..4].copy_from_slice(b"APOB");
        put_u32(&mut header, 0x08, 0x40_0001);
        put_u32(&mut header, 0x0C, 0x60);
        let dwords = dwords_at(base, &header);
        let calls = Arc::new(Mutex::new(Vec::new()));
        let seen = calls.clone();

        let mut reader = GraniteRidgeApobReader::with_read_dword(
            move |address| {
                seen.lock().unwrap().push(address);
                Ok(dwords.get(&address).copied().unwrap_or(0))
            },
            &[base],
            ApobLayout::Zen5,
        );
        assert!(reader.read().is_none());
        assert_eq!(*calls.lock().unwrap().iter().max().unwrap(), base + 0x0C);
        assert!(reader.last_error.contains("outside the APOB limit"));
    }

    #[test]
    fn rejects_header_that_changes_during_body_read() {
        let table = apob_table(None);
        let base = 0x4000;
        let dwords = dwords_at(base, &table);
        let base_reads = Arc::new(Mutex::new(0));

        let mut reader = GraniteRidgeApobReader::with_read_dword(
            move |address| {
                if address == base {
                    let mut count = base_reads.lock().unwrap();
                    *count += 1;
                    if *count >= 3 {
                        return Ok(u32::from_le_bytes(*b"BAD!"));
                    }
                }
                Ok(dwords.get(&address).copied().unwrap_or(0))
            },
            &[base],
            ApobLayout::Zen5,
        );
        assert!(reader.read().is_none());
        assert!(reader.last_error.contains("changed"));
    }

    #[test]
    fn rejects_multiple_valid_apob_candidate_bases() {
        let table = apob_table(None);
        let bases = [0x5000, 0x7000];
        let mut dwords = BTreeMap::new();
        for base in bases {
            dwords.extend(dwords_at(base, &table));
        }
        let mut reader = reader_over(dwords, &bases);
        assert!(reader.read().is_none());
        assert!(reader.last_error.contains("multiple"));
    }

    #[test]
    fn preserves_ambiguous_candidate_diagnostics() {
        let table = ambiguous_apob_table();
        let base = 0x6000;
        let mut reader = reader_over(dwords_at(base, &table), &[base]);

        assert!(reader.read().is_none());
        assert!(reader.last_error.contains("multiple"));
        assert_eq!(reader.ambiguous_candidates.len(), 1);
        let (address, diag) = &reader.ambiguous_candidates[0];
        assert_eq!(*address, base);
        assert_eq!(diag.candidates.iter().map(|c| c.record_offset).collect::<Vec<_>>(), vec![0xD0, 0xF4, 0x110]);
        assert_eq!(diag.sha256, sha(&table));
    }

    #[test]
    fn consensus_duplicates_do_not_leave_ambiguity_diagnostics() {
        let table = duplicate_real_apob_table(None, 0);
        let base = 0x6800;
        let mut reader = reader_over(dwords_at(base, &table), &[base]);

        let values = reader.read().unwrap();

        assert_eq!(values["proc_odt_pu"], "34.3 Ω");
        assert_eq!(reader.record_address, Some(base + 0xD0));
        assert!(reader.ambiguous_candidates.is_empty());
    }

    #[test]
    fn reader_exposes_exact_geometry_as_separate_channels() {
        let table = distinct_channel_apob_table();
        let base = 0x6C00;
        let mut reader = reader_over(dwords_at(base, &table), &[base]);

        let values = reader.read().unwrap();

        assert_eq!(values["rtt_wr"], "40 RZQ/6");
        assert_eq!(reader.channel_values["cha"]["rtt_wr"], "40 RZQ/6");
        assert_eq!(reader.channel_values["chb"]["rtt_wr"], "60 RZQ/4");
        assert_eq!(reader.channel_values["cha"]["ca_odt_a"], "480 RZQ/0.5");
        assert_eq!(reader.channel_values["chb"]["ca_odt_a"], "240 RZQ/1");
        assert_eq!(reader.channel_record_addresses["cha"], base + 0xD0);
        assert_eq!(reader.channel_record_addresses["chb"], base + 0x100);
        assert!(reader.ambiguous_candidates.is_empty());
    }

    #[test]
    fn reader_starts_with_no_ambiguous_candidates() {
        let reader = GraniteRidgeApobReader::with_read_dword(|_| Ok(0), &DEFAULT_APOB_ADDRESSES, ApobLayout::Zen5);
        assert!(reader.ambiguous_candidates.is_empty());
    }

    /// The shipped part of this file, without the tests.
    fn shipped_source() -> &'static str {
        include_str!("apob.rs").split("#[cfg(test)]").next().unwrap()
    }

    #[test]
    fn reader_exposes_no_ccdl_write_path() {
        // find_ccdl_wr only ever reads the table it is handed.
        // A Windows checkout may carry CRLF; the scan below wants LF.
        let source = shipped_source().replace('\r', "");
        let start = source.find("pub fn find_ccdl_wr(").unwrap();
        let body = &source[start..];
        let body = &body[..body.find("\n}\n").unwrap()];
        assert!(!body.to_lowercase().contains("write"));
    }

    #[test]
    fn reader_exposes_no_write_operation() {
        let source = shipped_source().to_lowercase();
        assert!(!source.contains("fn write"));
        assert!(!source.contains("set_phys_long"));
        assert!(!source.contains("setphyslong"));
    }

    #[test]
    fn a_driverless_live_reader_says_why() {
        // No hardware is installed under test: the live reader is refused
        // before it touches anything.
        let mut reader = GraniteRidgeApobReader::new(ApobLayout::Zen5);
        assert!(reader.read().is_none());
        assert_eq!(reader.last_error, "APOB physical reader unavailable: InpOut driver is not open");
    }

    // -- FindCcdlWrTest: tCCD_L_WR, anchored on the two tCCD_L values the
    //    UMC does report.

    fn ccdl_table(runs: &[&[u16]]) -> Vec<u8> {
        // 16-bit little-endian values, the way the APOB stores the run, with
        // unrelated data on both sides.
        let pad = 0x40u16;
        let mut data: Vec<u8> = (200..200 + pad).flat_map(u16::to_le_bytes).collect();
        for run in runs {
            data.extend(run.iter().flat_map(|value| value.to_le_bytes()));
            for _ in 0..8 {
                data.extend_from_slice(b"\x00\x11");
            }
        }
        data
    }

    #[test]
    fn the_value_between_the_two_anchors_is_returned() {
        // The bench run: 58, 8, 21, 58, 48, 60, 8, 21, 83, 42, 130, 58
        let table = ccdl_table(&[&[58, 8, 21, 58, 48, 60, 8, 21, 83, 42, 130, 58]]);
        assert_eq!(find_ccdl_wr(&table, Some(21), Some(42)), Some(83));
    }

    #[test]
    fn both_channel_runs_agreeing_is_still_one_answer() {
        let run: &[u16] = &[8, 21, 83, 42, 130];
        assert_eq!(find_ccdl_wr(&ccdl_table(&[run, run]), Some(21), Some(42)), Some(83));
    }

    #[test]
    fn disagreeing_matches_report_nothing() {
        // A coincidental 21/42 pair elsewhere must not be resolved by picking
        // one; without a single answer there is no answer.
        let table = ccdl_table(&[&[8, 21, 83, 42, 0], &[8, 21, 24, 42, 0]]);
        assert_eq!(find_ccdl_wr(&table, Some(21), Some(42)), None);
    }

    #[test]
    fn a_value_outside_the_ratio_bound_is_rejected() {
        assert_eq!(find_ccdl_wr(&ccdl_table(&[&[21, 400, 42]]), Some(21), Some(42)), None);
        assert_eq!(find_ccdl_wr(&ccdl_table(&[&[21, 3, 42]]), Some(21), Some(42)), None);
    }

    #[test]
    fn no_match_reports_nothing() {
        assert_eq!(find_ccdl_wr(&ccdl_table(&[&[8, 9, 10]]), Some(21), Some(42)), None);
    }

    #[test]
    fn missing_inputs_report_nothing() {
        let table = ccdl_table(&[&[21, 83, 42]]);
        assert_eq!(find_ccdl_wr(b"", Some(21), Some(42)), None);
        assert_eq!(find_ccdl_wr(&table, None, Some(42)), None);
        assert_eq!(find_ccdl_wr(&table, Some(21), None), None);
    }

    #[test]
    fn a_short_table_does_not_raise() {
        assert_eq!(find_ccdl_wr(b"\x15\x00\x53", Some(21), Some(42)), None);
    }

    // -- test_ccdl_run.py: the tCCD_L run, found by marker rather than by
    //    trusting the registers.
    //
    // ZenTimings moved all three tCCD_L readings out of the UMC registers
    // because some boards never program 0x50198 from the BIOS setting at all
    // -- so a register that disagrees with what was asked for is the
    // firmware's doing rather than a misread, and no amount of re-reading it
    // helps.
    //
    // The marker the run is found from is not something a BIOS can change,
    // which is the whole point: the run is located without needing the
    // registers to be right. On this bench the marker appears twice, once per
    // channel, and both runs read (21, 83, 42) -- the same values the
    // registers give here, which is what says adopting it changes nothing
    // where the registers are already correct.

    // The values this bench reads, from both the registers and both APOB copies.
    const BENCH_RUN: (u32, u32, u32) = (21, 83, 42);

    fn marker_bytes(marker: (u16, u16)) -> Vec<u8> {
        [marker.0.to_le_bytes(), marker.1.to_le_bytes()].concat()
    }

    /// A synthetic APOB carrying `copies` marked runs, with padding around.
    fn run_table(run: (u32, u32, u32), copies: usize) -> Vec<u8> {
        let padding = vec![0u8; 64];
        let mut table = Vec::new();
        for _ in 0..copies {
            let mut chunk = vec![0x11u8; CCDL_RUN_OFFSET * 2 + 8];
            chunk[..4].copy_from_slice(&marker_bytes(CCDL_RUN_MARKER));
            let start = CCDL_RUN_OFFSET * 2;
            for (index, value) in [run.0, run.1, run.2].into_iter().enumerate() {
                chunk[start + index * 2..start + index * 2 + 2].copy_from_slice(&(value as u16).to_le_bytes());
            }
            table.extend_from_slice(&padding);
            table.extend_from_slice(&chunk);
        }
        table.extend_from_slice(&padding);
        table
    }

    #[test]
    fn the_bench_run_is_found() {
        assert_eq!(find_ccdl_run(&run_table(BENCH_RUN, 2)), Some(BENCH_RUN));
    }

    #[test]
    fn one_copy_is_enough() {
        assert_eq!(find_ccdl_run(&run_table(BENCH_RUN, 1)), Some(BENCH_RUN));
    }

    #[test]
    fn channels_that_disagree_are_refused() {
        // The marker sits once per channel. Two different runs means it
        // matched somewhere it does not belong, and picking one of them is
        // worse than saying nothing.
        let mut table = run_table(BENCH_RUN, 2);
        let marker = marker_bytes(CCDL_RUN_MARKER);
        let second = table.windows(4).rposition(|window| window == marker).unwrap();
        let start = second + CCDL_RUN_OFFSET * 2;
        for (index, value) in [22u16, 84, 43].into_iter().enumerate() {
            table[start + index * 2..start + index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        assert_eq!(find_ccdl_run(&table), None);
    }

    #[test]
    fn a_table_without_the_marker_says_nothing() {
        assert_eq!(find_ccdl_run(&[0u8; 512]), None);
        assert_eq!(find_ccdl_run(b""), None);
    }

    #[test]
    fn a_run_running_past_the_table_is_refused() {
        // The marker can legitimately be the last thing in the blob.
        assert_eq!(find_ccdl_run(&marker_bytes(CCDL_RUN_MARKER)), None);
    }

    // PlausibilityTest: the encoding was derived empirically and may not hold
    // on every AGESA.

    #[test]
    fn a_tccdl_outside_its_range_is_dropped() {
        assert_eq!(find_ccdl_run(&run_table((2, 83, 42), 2)), None);
        assert_eq!(find_ccdl_run(&run_table((99, 83, 42), 2)), None);
    }

    #[test]
    fn a_wr2_outside_its_range_is_dropped() {
        assert_eq!(find_ccdl_run(&run_table((21, 83, 900), 2)), None);
    }

    #[test]
    fn wr_below_tccdl_or_above_four_times_it_is_dropped() {
        assert_eq!(find_ccdl_run(&run_table((21, 20, 42), 2)), None);
        assert_eq!(find_ccdl_run(&run_table((21, 200, 42), 2)), None);
    }

    #[test]
    fn the_boundaries_themselves_are_accepted() {
        // tCCD_L_WR exactly equal to tCCD_L, and exactly four times it.
        assert_eq!(find_ccdl_run(&run_table((21, 21, 42), 2)), Some((21, 21, 42)));
        assert_eq!(find_ccdl_run(&run_table((21, 84, 42), 2)), Some((21, 84, 42)));
    }

    // -- test_am5_misc_settings.py: the raw training codes ----------------

    #[test]
    fn raw_training_codes_follow_bytes_and_preserve_known_fields() {
        let mut block = vec![0u8; 0x30];
        block[0x10..0x17].copy_from_slice(&[9, 40, 60, 30, 91, 82, 43]);
        let values = decode_granite_ridge_training_block(&block).unwrap();
        let raw: Vec<TrainingValue> =
            ["ALERT_PU", "CA_DRV", "PHY_VREF", "DQ_VREF", "CA_VREF", "CS_VREF", "RX_DFE", "TX_DFE"]
                .iter()
                .map(|name| values[name].clone())
                .collect();
        let expected: Vec<TrainingValue> = [9, 9, 40, 60, 30, 91, 82, 43].into_iter().map(TrainingValue::Raw).collect();
        assert_eq!(raw, expected);
        assert_eq!(values["proc_ca_ds"], "40 Ω");
        block[0x14] = 0;
        assert_eq!(decode_granite_ridge_training_block(&block).unwrap()["CS_VREF"], 0u8);
        assert!(decode_granite_ridge_training_block(&block[..0x16]).is_none());
    }

    // -- test_raphael.py: experimental desktop Ryzen 7000 (Raphael) -------
    //
    // No Raphael capture exists in this project. These tests pin the
    // ZenStates-Core layouts the code was written from and the checks that
    // keep a wrong layout from reaching the screen; they cannot say the
    // layouts are right.

    fn raphael_block(overrides: &[(usize, u8)]) -> Vec<u8> {
        let mut block = vec![0u8; RAPHAEL_BLOCK_SIZE];
        block[0x00] = 1;
        block[0x02..0x07].copy_from_slice(&[0, 0, 6, 5, 7]); // RTT: off, off, 40, 48, 34
        block[0x07] = 1; // DRAM DQ DS 40
        block[0x08..0x0E].copy_from_slice(&[0, 0, 1, 5, 5, 5]);
        block[0x0E] = 28; // Proc ODT 40
        block[0x0F] = 30; // Proc DQ DS 34.3
        block[0x11] = 40; // Proc CA DS 40
        for (offset, value) in overrides {
            block[*offset] = *value;
        }
        block
    }

    /// An APOB container in the geometry both parsers walk.
    fn raphael_table(block: Option<&[u8]>, extended: Option<&[u8]>) -> Vec<u8> {
        let default = raphael_block(&[]);
        let block = block.unwrap_or(&default);
        let mut table = vec![0u8; 0x200];
        table[0..4].copy_from_slice(b"APOB");
        let length = table.len() as u32;
        put_u32(&mut table, 4, 1);
        put_u32(&mut table, 8, length);
        put_u32(&mut table, 12, 0x60);
        let first = 0x80;
        put_u32(&mut table, 0x30, first as u32);
        put_u32(&mut table, first + 0x0C, 0x20);
        let main = first + 0x20;
        table[main] = 0x01;
        table[main + 4] = 0x19;
        put_u32(&mut table, main + 0x0C, 0x60);
        table[main + 0x30..main + 0x30 + block.len()].copy_from_slice(block);
        if let Some(extended) = extended {
            let ext = 0x140;
            put_u32(&mut table, 0x34, ext as u32);
            table[ext] = 0x07;
            table[ext + 4] = 0x03;
            put_u32(&mut table, ext + 0x0C, 0x40);
            table[ext + 0x10..ext + 0x10 + extended.len()].copy_from_slice(extended);
        }
        table
    }

    #[test]
    fn the_block_decodes_with_the_zen4_offsets() {
        let values = decode_raphael_training_block(&raphael_block(&[]), None).unwrap();
        assert_eq!(values["rtt_wr"], "40 RZQ/6");
        assert_eq!(values["rtt_park"], "48 RZQ/5");
        assert_eq!(values["rtt_park_dqs"], "34 RZQ/7");
        assert_eq!(values["rtt_nom_rd"], "Off");
        assert_eq!(values["dram_dq_ds"], "40 Ω");
        assert_eq!(values["proc_odt"], "40 Ω");
        assert_eq!(values["proc_ca_ds"], "40 Ω");
        assert_eq!(values["ck_odt_a"], "Off");
        // Zen 5's pull-up/pull-down pairs have no Zen 4 counterpart and are
        // left out rather than guessed.
        assert!(!values.contains_key("proc_odt_pu"));
        assert!(!values.contains_key("dram_dq_ds_pu"));
    }

    #[test]
    fn the_table_parse_finds_the_record_after_the_lead() {
        let parsed = parse_raphael_apob_table(&raphael_table(None, None)).unwrap();
        assert_eq!(parsed.record_offset, 0xA0 + 0x30);
        assert_eq!(parsed.values["rtt_wr"], "40 RZQ/6");
        assert!(!parsed.values.contains_key("proc_ck_ds"));
    }

    #[test]
    fn the_extended_block_adds_ck_and_cs_drive() {
        let mut extended = raphael_block(&[]);
        extended[0x12] = 60;
        extended[0x13] = 30;
        let parsed = parse_raphael_apob_table(&raphael_table(None, Some(&extended))).unwrap();
        assert_eq!(parsed.values["proc_ck_ds"], "60 Ω");
        assert_eq!(parsed.values["proc_cs_ds"], "30 Ω");
    }

    #[test]
    fn an_implausible_block_is_refused() {
        for bad in [
            raphael_block(&[(0x04, 9)]),  // RTT code out of range
            raphael_block(&[(0x07, 5)]),  // no such DRAM drive
            raphael_block(&[(0x11, 99)]), // no such CA drive
            raphael_block(&[(0x02, 0), (0x03, 0), (0x04, 0), (0x05, 0), (0x06, 0)]),
        ] {
            assert!(parse_raphael_apob_table(&raphael_table(Some(&bad), None)).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_ryzen_9000_table_does_not_decode_as_raphael() {
        assert!(parse_raphael_apob_table(&apob_table(None)).is_err());
    }

    #[test]
    fn the_reader_uses_the_zen4_layout_when_asked() {
        let table = raphael_table(None, None);
        let mut apob = GraniteRidgeApobReader::with_read_dword(
            move |address| {
                let offset = address.wrapping_sub(0x0A20_0000) as usize;
                Ok(if offset + 4 <= table.len() {
                    u32::from_le_bytes(table[offset..offset + 4].try_into().unwrap())
                } else {
                    0
                })
            },
            &DEFAULT_APOB_ADDRESSES,
            ApobLayout::Zen4,
        );
        let values = apob.read();
        assert!(values.is_some(), "{}", apob.last_error);
        assert_eq!(values.unwrap()["rtt_park"], "48 RZQ/5");
        assert!(apob.channel_values.is_empty());
        assert_eq!(apob.raw_record.len(), RAPHAEL_BLOCK_SIZE);
    }

    #[test]
    fn an_unknown_layout_is_refused() {
        assert_eq!(ApobLayout::from_name("zen3").unwrap_err(), "unknown APOB layout 'zen3'");
        assert_eq!(ApobLayout::from_name("zen4"), Ok(ApobLayout::Zen4));
        assert_eq!(ApobLayout::Zen5.name(), "zen5");
    }
}
