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

//! Decoder for AMD AM5 (Zen 4 / Granite Ridge) UMC timing registers.
//!
//! The UMC register block for each channel lives at an SMN base of 0x0 and
//! 0x100000; every timing register offset below is relative to that base.
//!
//! [`decode_channel`] is a pure function over an `{offset: value}` snapshot
//! so it can be unit-tested without any hardware access. [`read_channel`]
//! pulls a live snapshot through an [`SmnTransport`] and decodes it,
//! returning `None` for implausible / failed snapshots.

use std::collections::BTreeMap;
use std::fmt;

use super::smn::SmnTransport;
use crate::pyfmt::float_str;

/// A register snapshot: offset (relative to the channel base) -> value.
pub type Registers = BTreeMap<u32, u32>;

// UMC channel SMN bases.
pub const UMC_BASES: [u32; 2] = [0x0, 0x100000];

// Register offsets (relative to a channel base).
pub const REG_BGS0: u32 = 0x50050; // bank-group-swap pattern register 0
pub const REG_BGS1: u32 = 0x50058; // bank-group-swap pattern register 1
pub const REG_BGSA0: u32 = 0x500D0; // bank-group-swap-alt register 0
pub const REG_BGSA1: u32 = 0x500D4; // bank-group-swap-alt register 1
pub const REG_CONFIG: u32 = 0x50200; // ratio[15:0], Cmd2T bit17, GDM bit18
pub const REG_DRAM_CONFIG: u32 = 0x50100; // DimmEccEn bit12 (ZenStates-Core DDR5Dictionary)
pub const REG_RCD: u32 = 0x50204; // CL[5:0], RAS[14:8], RCDRD[21:16], RCDWR[29:24]
pub const REG_RP_RC: u32 = 0x50208; // RP[21:16], RC[7:0]
pub const REG_RTP_RRD: u32 = 0x5020C; // RTP[28:24], RRDL[12:8], RRDS[4:0]
pub const REG_FAW: u32 = 0x50210; // FAW[7:0]
pub const REG_WTR_CWL: u32 = 0x50214; // WTRL[22:16], WTRS[12:8], CWL[5:0]
pub const REG_WR: u32 = 0x50218; // WR[7:0]
pub const REG_RDRD: u32 = 0x50220; // tertiary read-to-read
pub const REG_WRWR: u32 = 0x50224; // tertiary write-to-write
pub const REG_RDWR: u32 = 0x50228; // read<->write turnaround
pub const REG_REFI: u32 = 0x50230; // REFI[15:0]
pub const REG_MOD: u32 = 0x50234; // MODPDA[29:24], MRDPDA[21:16], MOD[13:8], MRD[5:0]
pub const REG_STAG: u32 = 0x50250; // STAG[26:16], STAGsb[8:0]
pub const REG_CKE_XP: u32 = 0x50254; // CKE[28:24], XP[5:0]
pub const REG_PHY: u32 = 0x50258; // PHYWRD[26:24], PHYRDL[23:16], PHYWRL[15:8]
pub const REG_NITRO: u32 = 0x50284; // Nitro dials: Rx[9:8], Tx[5:4], Ctrl[1:0]
pub const REG_PRE: u32 = 0x502A4; // WRPOST/WRPRE/RDPOST/RDPRE (+1 correction on PRE)
pub const REG_PD: u32 = 0x5012C; // powerdown enable bit28
pub const REG_CCDL: u32 = 0x50198; // tCCD_L[7:3] + 5
pub const REG_CCDL_WR2: u32 = 0x502E0; // tCCD_L_WR2[5:0] + 7

// Both tCCD_L registers store a biased value, so an all-ones (failed) read
// decodes to a plausible-looking number rather than an obvious one. These are
// the ranges outside which the decode is rejected instead of displayed; they
// match the bounds ZenTimings applies to the same two registers.
pub const CCDL_RANGE: (i64, i64) = (8, 36);
pub const CCDL_WR2_RANGE: (i64, i64) = (8, 70);

// tRFC / tRFC2 candidates; the first active dword contains both values.
pub const RFC_OFFSETS: [u32; 4] = [0x50260, 0x50264, 0x50268, 0x5026C];
pub const RFC_PLACEHOLDER: u32 = 0x00C0_0138;

// tRFCsb candidates; first with nonzero low-11 wins.
pub const RFCSB_OFFSETS: [u32; 4] = [0x502C0, 0x502C4, 0x502C8, 0x502CC];

// Every offset read for a live snapshot.
pub const ALL_OFFSETS: [u32; 33] = [
    REG_BGS0, REG_BGS1, REG_BGSA0, REG_BGSA1,
    REG_CONFIG, REG_DRAM_CONFIG, REG_RCD, REG_RP_RC, REG_RTP_RRD,
    REG_FAW, REG_WTR_CWL,
    REG_WR, REG_RDRD, REG_WRWR, REG_RDWR, REG_REFI, REG_MOD, REG_STAG,
    REG_CKE_XP, REG_PHY, REG_NITRO, REG_PRE, REG_PD,
    REG_CCDL, REG_CCDL_WR2,
    RFC_OFFSETS[0], RFC_OFFSETS[1], RFC_OFFSETS[2], RFC_OFFSETS[3],
    RFCSB_OFFSETS[0], RFCSB_OFFSETS[1], RFCSB_OFFSETS[2], RFCSB_OFFSETS[3],
];

const INVALID: u32 = 0xFFFF_FFFF;
const BGS_DISABLED_PATTERN: u32 = 0x8765_4321;

/// One decoded value, of whichever type the Python dict held.
#[derive(Clone, Debug, PartialEq)]
pub enum UmcValue {
    Int(i64),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl UmcValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            UmcValue::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// The value as a number: Python's `float(value)` on an int or float.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            UmcValue::Int(value) => Some(*value as f64),
            UmcValue::Float(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            UmcValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            UmcValue::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// Python's `str(value)`: `True`/`False` for a flag, a float with its `.0`.
impl fmt::Display for UmcValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UmcValue::Int(value) => write!(f, "{value}"),
            UmcValue::Float(value) => f.write_str(&float_str(*value)),
            UmcValue::Bool(true) => f.write_str("True"),
            UmcValue::Bool(false) => f.write_str("False"),
            UmcValue::Text(value) => f.write_str(value),
        }
    }
}

/// A decoded channel: every key the Python dict had, `None` where the
/// Python held `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DecodedChannel(pub BTreeMap<&'static str, Option<UmcValue>>);

impl DecodedChannel {
    /// Python's `decoded.get(name)`: `None` for a missing key and for a key
    /// holding `None` alike.
    pub fn get(&self, name: &str) -> Option<&UmcValue> {
        self.0.get(name).and_then(Option::as_ref)
    }

    pub fn contains_key(&self, name: &str) -> bool {
        self.0.contains_key(name)
    }

    fn set(&mut self, name: &'static str, value: Option<UmcValue>) {
        self.0.insert(name, value);
    }
}

fn bits(value: u32, low: u32, width: u32) -> u32 {
    (value >> low) & ((1u32 << width) - 1)
}

fn reg(regs: &Registers, offset: u32) -> u32 {
    regs.get(&offset).copied().unwrap_or(0)
}

fn int(value: u32) -> Option<UmcValue> {
    Some(UmcValue::Int(value as i64))
}

/// Decode a biased tCCD_L field, or None when it is out of range.
///
/// Missing register, failed read or a value outside the range the controller
/// can hold all mean the same thing to a reader: this is not a timing. None
/// surfaces as an em-dash rather than as a number that looks real.
fn biased_field(regs: &Registers, offset: u32, low: u32, width: u32, bias: i64, valid_range: (i64, i64)) -> Option<i64> {
    let raw = *regs.get(&offset)?;
    if raw == INVALID {
        return None;
    }
    let value = bits(raw, low, width) as i64 + bias;
    let (low_bound, high_bound) = valid_range;
    if !(low_bound <= value && value <= high_bound) {
        return None;
    }
    Some(value)
}

fn first_rfc_pair(regs: &Registers, offsets: &[u32]) -> (u32, u32) {
    for off in offsets {
        let val = reg(regs, *off);
        if val != 0 && val != RFC_PLACEHOLDER {
            return (val & 0xFFFF, (val >> 16) & 0xFFFF);
        }
    }
    (0, 0)
}

fn first_rfcsb(regs: &Registers, offsets: &[u32]) -> u32 {
    for off in offsets {
        let low = reg(regs, *off) & 0x7FF;
        if low != 0 {
            return low;
        }
    }
    0
}

/// Bank-group-swap-alt, a separate control from BGS.
///
/// Enabled when either alt register has a nonzero field at bits 4..10; the
/// pattern comparison used for BGS does not apply here. Register numbers and
/// the bit field follow the ZenStates-Core BaseDramTimings reference.
fn decode_bgs_alt(regs: &Registers) -> Option<bool> {
    let (Some(alt0), Some(alt1)) = (regs.get(&REG_BGSA0), regs.get(&REG_BGSA1)) else {
        return None;
    };
    if *alt0 == INVALID || *alt1 == INVALID {
        return None;
    }
    Some(bits(*alt0, 4, 7) != 0 || bits(*alt1, 4, 7) != 0)
}

fn decode_bgs(regs: &Registers) -> Option<bool> {
    let (Some(bgs0), Some(bgs1)) = (regs.get(&REG_BGS0), regs.get(&REG_BGS1)) else {
        return None;
    };
    if *bgs0 == INVALID || *bgs1 == INVALID {
        return None;
    }
    Some(!(*bgs0 == BGS_DISABLED_PATTERN && *bgs1 == BGS_DISABLED_PATTERN))
}

fn decode_refresh_mode(value: u32) -> &'static str {
    let fgr = bits(value, 16, 3);
    let per_bank = bits(value, 1, 1) != 0;
    if per_bank {
        return if fgr != 0 { "Mixed" } else { "Per-Bank Only" };
    }
    if fgr != 0 { "FGR" } else { "Normal" }
}

/// Reject 0xFFFFFFFF / saturated / empty snapshots.
fn is_plausible(regs: &Registers) -> bool {
    let rcd = reg(regs, REG_RCD);
    let config = reg(regs, REG_CONFIG);
    let mclk_mhz = bits(config, 0, 16);
    if !(1000..=6000).contains(&mclk_mhz) {
        return false;
    }
    if rcd == 0 || rcd == INVALID {
        return false;
    }
    let cl = bits(rcd, 0, 6);
    if cl == 0 || cl >= 0x3F {
        // 0 or saturated
        return false;
    }
    let ras = bits(rcd, 8, 7);
    if ras == 0 || ras >= 0x7F {
        return false;
    }
    for off in [REG_RP_RC, REG_RTP_RRD, REG_WTR_CWL] {
        if reg(regs, off) == INVALID {
            return false;
        }
    }
    true
}

/// Decode an `{offset: value}` snapshot into named timings.
///
/// Returns `None` if the snapshot is missing, saturated or otherwise
/// implausible.
pub fn decode_channel(regs: &Registers) -> Option<DecodedChannel> {
    if !is_plausible(regs) {
        return None;
    }

    let config = reg(regs, REG_CONFIG);
    let rcd = reg(regs, REG_RCD);
    let rp_rc = reg(regs, REG_RP_RC);
    let rtp = reg(regs, REG_RTP_RRD);
    let wtr = reg(regs, REG_WTR_CWL);
    let module = reg(regs, REG_MOD);
    let stag = reg(regs, REG_STAG);
    let cke = reg(regs, REG_CKE_XP);
    let phy = reg(regs, REG_PHY);
    let rdrd = reg(regs, REG_RDRD);
    let wrwr = reg(regs, REG_WRWR);
    let rdwr = reg(regs, REG_RDWR);
    let nitro = reg(regs, REG_NITRO);
    let pre = reg(regs, REG_PRE);
    let pd = reg(regs, REG_PD);

    let ratio = bits(config, 0, 16);

    let (rfc, rfc2) = first_rfc_pair(regs, &RFC_OFFSETS);
    let rfcsb = first_rfcsb(regs, &RFCSB_OFFSETS);
    let refresh_mode = decode_refresh_mode(pd);
    // Outside Normal mode the controller refreshes on tRFC2, so tRFC itself is
    // not the interval in effect.
    let active_rfc = if refresh_mode == "Normal" { rfc } else { rfc2 };

    let to_ns = |cycles: u32| -> Option<UmcValue> {
        (cycles != 0 && ratio != 0).then(|| UmcValue::Float(cycles as f64 * 1000.0 / ratio as f64))
    };

    let rfc_ns = to_ns(active_rfc);
    let rfcsb_ns = to_ns(rfcsb);
    let rdpre_raw = bits(pre, 0, 3);

    let mut decoded = DecodedChannel::default();
    let mut put = |name: &'static str, value: Option<UmcValue>| decoded.set(name, value);

    put("ratio", int(ratio));
    put("mclk_mhz", int(ratio));
    put("cmd_rate", Some(UmcValue::Text(if bits(config, 17, 1) != 0 { "2T" } else { "1T" }.into())));
    put("gdm", Some(UmcValue::Bool(bits(config, 18, 1) != 0)));
    put("bgs", decode_bgs(regs).map(UmcValue::Bool));
    put("bgs_alt", decode_bgs_alt(regs).map(UmcValue::Bool));

    put("tCL", int(bits(rcd, 0, 6)));
    put("tRAS", int(bits(rcd, 8, 7)));
    put("tRCDRD", int(bits(rcd, 16, 6)));
    put("tRCDWR", int(bits(rcd, 24, 6)));

    put("tRP", int(bits(rp_rc, 16, 6)));
    put("tRC", int(bits(rp_rc, 0, 8)));

    put("tRTP", int(bits(rtp, 24, 5)));
    put("tRRD_L", int(bits(rtp, 8, 5)));
    put("tRRD_S", int(bits(rtp, 0, 5)));

    put("tFAW", int(bits(reg(regs, REG_FAW), 0, 8)));

    put("tCWL", int(bits(wtr, 0, 6)));
    put("tWTR_S", int(bits(wtr, 8, 5)));
    put("tWTR_L", int(bits(wtr, 16, 7)));

    put("tWR", int(bits(reg(regs, REG_WR), 0, 8)));

    // tertiary / turnaround
    put("tRDRDSCL", int(bits(rdrd, 24, 6)));
    put("tRDRDSC", int(bits(rdrd, 16, 4)));
    put("tRDRDSD", int(bits(rdrd, 8, 4)));
    put("tRDRDDD", int(bits(rdrd, 0, 4)));
    put("tWRWRSCL", int(bits(wrwr, 24, 6)));
    put("tWRWRSC", int(bits(wrwr, 16, 4)));
    put("tWRWRSD", int(bits(wrwr, 8, 4)));
    put("tWRWRDD", int(bits(wrwr, 0, 4)));
    put("tRDWR", int(bits(rdwr, 8, 6)));
    put("tWRRD", int(bits(rdwr, 0, 4)));

    // tCCD_L sits outside the 0x502xx timing block and is stored biased.
    // tCCD_L_WR is in neither register; see amd::apob::find_ccdl_wr.
    put("tCCD_L", biased_field(regs, REG_CCDL, 3, 5, 5, CCDL_RANGE).map(UmcValue::Int));
    put("tCCD_L_WR2", biased_field(regs, REG_CCDL_WR2, 0, 6, 7, CCDL_WR2_RANGE).map(UmcValue::Int));

    put("tREFI", int(bits(reg(regs, REG_REFI), 0, 16)));
    put("tRFC", int(rfc));
    put("tRFC2", int(rfc2));
    put("tRFCsb", int(rfcsb));
    put("tRFCsb_ns", rfcsb_ns);
    put("tRFC_ns", rfc_ns);
    put("refresh_mode", Some(UmcValue::Text(refresh_mode.into())));
    // The raw fine-granularity-refresh field, which refresh_mode above folds
    // into a word along with the per-bank bit. Both are reported because they
    // answer different questions: the mode says what the controller is doing,
    // this says which FGR level it was set to, and "Mixed" alone cannot be
    // read back to a setting.
    put("fgr", int(bits(pd, 16, 3)));

    put("tMRD", int(bits(module, 0, 6)));
    put("tMOD", int(bits(module, 8, 6)));
    put("tMRDPDA", int(bits(module, 16, 6)));
    put("tMODPDA", int(bits(module, 24, 6)));

    put("tSTAG", int(bits(stag, 16, 11)));
    put("tSTAGsb", int(bits(stag, 0, 9)));
    put("tCKE", int(bits(cke, 24, 5)));
    put("tXP", int(bits(cke, 0, 6)));
    put("tPHYWRD", int(bits(phy, 24, 3)));
    put("tPHYRDL", int(bits(phy, 16, 8)));
    put("tPHYWRL", int(bits(phy, 8, 8)));

    // AMD encodes WRPRE zero-based. RDPRE encodings 0/1 are zero-based;
    // values >=2 are already literal in the validated DDR5 UMC layout.
    put("tWRPOST", int(bits(pre, 12, 3)));
    put("tWRPRE", int(bits(pre, 8, 3) + 1));
    put("tRDPOST", int(bits(pre, 4, 3)));
    put("tRDPRE", int(if rdpre_raw < 2 { rdpre_raw + 1 } else { rdpre_raw }));

    put("nitro_rx", int(bits(nitro, 8, 2)));
    put("nitro_tx", int(bits(nitro, 4, 2)));
    put("nitro_ctrl", int(bits(nitro, 0, 2)));

    put("powerdown", Some(UmcValue::Bool(bits(pd, 28, 1) != 0)));

    for (name, value) in decode_misc_settings(regs) {
        put(name, value.map(UmcValue::Text));
    }
    Some(decoded)
}

/// The five controller settings [`decode_misc_settings`] reports, in order.
pub const MISC_SETTINGS: [&str; 5] = ["read_preamble", "write_preamble", "read_postamble", "write_postamble", "ecc"];

/// UMC settings, not constants inferred from module type or another app.
///
/// Field locations follow ZenStates-Core Dictionaries/DDR5Dictionary.cs;
/// preamble count corrections follow Hardware/DRAM/BaseDramTimings.cs.
/// Postamble code/duration mappings were checked against the supplied
/// reference 1.0.0's UMC field descriptors and read/write formatter tables.
/// Unknown encodings remain explicit rather than extrapolating a duration.
/// DimmEccEn is controller ECC, not DDR5's internal on-die ECC.
pub fn decode_misc_settings(regs: &Registers) -> BTreeMap<&'static str, Option<String>> {
    let mut result: BTreeMap<&'static str, Option<String>> = MISC_SETTINGS.iter().map(|name| (*name, None)).collect();
    if let Some(&raw) = regs.get(&REG_PRE)
        && raw != INVALID
    {
        let (rd, wr) = (bits(raw, 0, 3), bits(raw, 8, 3));
        let rd_count = if rd < 2 { rd + 1 } else { rd };
        let wr_count = wr + 1;
        let mut read_preamble = if (1..=4).contains(&rd_count) {
            format!("{rd_count} tCK")
        } else {
            format!("Reserved (UMC {rd})")
        };
        let mut write_preamble = if (1..=4).contains(&wr_count) {
            format!("{wr_count} tCK")
        } else {
            format!("Reserved (UMC {wr})")
        };
        // Preserve the two distinct read-preamble encodings for 2 tCK.
        // Patterns follow the DDR5 reference formatter, not the cycle count.
        let rd_pattern = match rd {
            0 => Some("10"),
            1 => Some("0010"),
            2 => Some("1110"),
            3 => Some("000010"),
            4 => Some("00001010"),
            _ => None,
        };
        let wr_pattern = match wr {
            1 => Some("0010"),
            2 => Some("000010"),
            3 => Some("00001010"),
            _ => None,
        };
        if let Some(pattern) = rd_pattern {
            read_preamble += &format!(" - {pattern} Pattern");
        }
        if let Some(pattern) = wr_pattern {
            write_preamble += &format!(" - {pattern} Pattern");
        }
        result.insert("read_preamble", Some(read_preamble));
        result.insert("write_preamble", Some(write_preamble));
        for (name, shift, long_pattern) in [("read_postamble", 4, "010"), ("write_postamble", 12, "000")] {
            let code = bits(raw, shift, 3);
            let text = match code {
                0 => "0.5 tCK - 0 Pattern".to_string(),
                1 => format!("1.5 tCK - {long_pattern} Pattern"),
                _ => format!("Reserved (UMC {code})"),
            };
            result.insert(name, Some(text));
        }
    }
    if let Some(&config) = regs.get(&REG_DRAM_CONFIG)
        && config != INVALID
    {
        result.insert("ecc", Some(if bits(config, 12, 1) != 0 { "Enabled" } else { "Disabled" }.to_string()));
    }
    result
}

/// Read and decode one UMC channel through `reader`.
///
/// `reader.read(smn_address)` must return the 32-bit value or `None`.
/// Returns the decoded channel, or `None` for a failed / implausible read.
pub fn read_channel(reader: &dyn SmnTransport, base: u32) -> Option<DecodedChannel> {
    let mut regs = Registers::new();
    for off in ALL_OFFSETS {
        if let Some(val) = reader.read(base + off) {
            regs.insert(off, val);
        }
    }
    decode_channel(&regs)
}

#[cfg(test)]
mod tests {
    //! Decode tests for the AM5 UMC timing registers.
    //!
    //! The tests synthesize register snapshots from an oracle set of timings,
    //! feed them through the pure decoder, and assert the decoded values
    //! match. No hardware is involved.

    use super::*;

    /// Encode the oracle timing set into UMC register values.
    fn oracle_regs() -> Registers {
        let mut regs = Registers::new();
        // 0x50200: ratio[15:0], Cmd2T bit17, GDM bit18
        regs.insert(0x50200, 4100); // MCLK 4100, 1T, GDM off
        // 0x50204: CL[5:0], RAS[14:8], RCDRD[21:16], RCDWR[29:24]
        regs.insert(0x50204, 36 | (58 << 8) | (46 << 16) | (8 << 24));
        // 0x50208: RP[21:16], RC[7:0]
        regs.insert(0x50208, 104 | (46 << 16));
        // 0x5020C: RTP[28:24], RRDL[12:8], RRDS[4:0]
        regs.insert(0x5020C, 8 | (8 << 8) | (12 << 24));
        // 0x50210: FAW[7:0]
        regs.insert(0x50210, 32);
        // 0x50214: WTRL[22:16], WTRS[12:8], CWL[5:0]
        regs.insert(0x50214, 34 | (4 << 8) | (18 << 16));
        // 0x50218: WR[7:0]
        regs.insert(0x50218, 48);
        // 0x50230: REFI[15:0]
        regs.insert(0x50230, 65535);
        // tRFC / tRFC2: first nonzero, non-0x00C00138 in 0x50260/64/68/6C
        regs.insert(0x50260, 0x00C0_0138); // placeholder -> skipped
        regs.insert(0x50264, 0); // skipped
        regs.insert(0x50268, (480 << 16) | 480); // one active dword: tRFC2 | tRFC
        regs.insert(0x5026C, 0);
        // tRFCsb: first nonzero low-11 in 0x502C0/C4/C8/CC
        regs.insert(0x502C0, 0);
        regs.insert(0x502C4, 390);
        // Nitro 0x50284: Rx=1, Tx=3, Ctrl=1
        regs.insert(0x50284, 1 | (3 << 4) | (1 << 8));
        // powerdown 0x5012C bit28
        regs.insert(0x5012C, 0);
        // 0x50198: tCCD_L[7:3] + 5, and 0x502E0: tCCD_L_WR2[5:0] + 7.
        // Raw values captured on the bench, where ZenTimings reads 21 and 42.
        regs.insert(0x50198, 0x1B01_1385);
        regs.insert(0x502E0, 0x0000_0023);
        regs
    }

    fn decoded() -> DecodedChannel {
        decode_channel(&oracle_regs()).expect("the oracle decodes")
    }

    fn num(d: &DecodedChannel, name: &str) -> i64 {
        d.get(name).and_then(UmcValue::as_i64).unwrap_or_else(|| panic!("{name} is not an int"))
    }

    fn float(d: &DecodedChannel, name: &str) -> f64 {
        d.get(name).and_then(UmcValue::as_f64).unwrap_or_else(|| panic!("{name} is not a number"))
    }

    fn text<'a>(d: &'a DecodedChannel, name: &str) -> &'a str {
        d.get(name).and_then(UmcValue::as_str).unwrap_or_else(|| panic!("{name} is not text"))
    }

    fn flag(d: &DecodedChannel, name: &str) -> bool {
        d.get(name).and_then(UmcValue::as_bool).unwrap_or_else(|| panic!("{name} is not a flag"))
    }

    fn with(changes: &[(u32, u32)]) -> DecodedChannel {
        let mut regs = oracle_regs();
        regs.extend(changes.iter().copied());
        decode_channel(&regs).expect("decodes")
    }

    fn assert_close(actual: f64, expected: f64) {
        // assertAlmostEqual: seven decimal places.
        assert!((actual - expected).abs() < 5e-8, "{actual} != {expected}");
    }

    // -- DecodeTest ----------------------------------------------------------

    #[test]
    fn snapshot_is_valid() {
        assert!(decode_channel(&oracle_regs()).is_some());
    }

    #[test]
    fn primary_timings() {
        let d = decoded();
        assert_eq!(num(&d, "tCL"), 36);
        assert_eq!(num(&d, "tRAS"), 58);
        assert_eq!(num(&d, "tRCDRD"), 46);
        assert_eq!(num(&d, "tRCDWR"), 8);
        assert_eq!(num(&d, "tRP"), 46);
        assert_eq!(num(&d, "tRC"), 104);
    }

    #[test]
    fn secondary_timings() {
        let d = decoded();
        assert_eq!(num(&d, "tRTP"), 12);
        assert_eq!(num(&d, "tRRD_L"), 8);
        assert_eq!(num(&d, "tRRD_S"), 8);
        assert_eq!(num(&d, "tFAW"), 32);
        assert_eq!(num(&d, "tCWL"), 34);
        assert_eq!(num(&d, "tWTR_L"), 18);
        assert_eq!(num(&d, "tWTR_S"), 4);
        assert_eq!(num(&d, "tWR"), 48);
    }

    #[test]
    fn refresh_timings() {
        let d = decoded();
        assert_eq!(num(&d, "tREFI"), 65535);
        assert_eq!(num(&d, "tRFC"), 480);
        assert_eq!(num(&d, "tRFC2"), 480);
        assert_eq!(num(&d, "tRFCsb"), 390);
    }

    #[test]
    fn trfc_nanoseconds_uses_active_refresh_timing() {
        let normal = with(&[(0x50200, 4000), (0x50268, (600 << 16) | 500), (0x5012C, 0)]);
        assert_close(float(&normal, "tRFC_ns"), 125.0);

        let mixed = with(&[(0x50200, 4000), (0x50268, (600 << 16) | 500), (0x5012C, (1 << 16) | (1 << 1))]);
        assert_close(float(&mixed, "tRFC_ns"), 150.0);
    }

    #[test]
    fn real_granite_ridge_trfc_nanoseconds() {
        let d = with(&[(0x5012C, 0x0539_114A)]);
        assert_close(float(&d, "tRFC_ns"), 117.0731707317);
    }

    #[test]
    fn config_bits() {
        let d = decoded();
        assert_eq!(num(&d, "ratio"), 4100);
        assert_eq!(num(&d, "mclk_mhz"), 4100);
        assert_eq!(text(&d, "cmd_rate"), "1T");
        assert!(!flag(&d, "gdm"));
        assert!(!flag(&d, "powerdown"));
    }

    #[test]
    fn refresh_mode_decodes_fgr_and_per_bank_bits() {
        let cases = [(0, "Normal"), (1 << 16, "FGR"), (1 << 1, "Per-Bank Only"), ((1 << 16) | (1 << 1), "Mixed")];
        for (raw, expected) in cases {
            let d = with(&[(0x5012C, raw)]);
            assert_eq!(text(&d, "refresh_mode"), expected, "raw={raw}");
        }
    }

    #[test]
    fn real_granite_ridge_refresh_mode_is_mixed() {
        assert_eq!(text(&with(&[(0x5012C, 0x0539_114A)]), "refresh_mode"), "Mixed");
    }

    #[test]
    fn fgr_is_reported_as_its_own_level() {
        // "Mixed" cannot be read back to a setting: it says FGR is on and
        // per-bank is on, not which FGR level was chosen. This is that level,
        // and the same register the mode above comes from.
        let d = with(&[(0x5012C, 0x0539_114A)]);
        // ZenTimings shows 1 on this board, reading the same three bits.
        assert_eq!(num(&d, "fgr"), 1);
        assert_eq!(text(&d, "refresh_mode"), "Mixed");
    }

    #[test]
    fn fgr_reads_the_whole_three_bit_field() {
        // A one-bit read would call level 4 zero and quietly say Normal.
        for level in 0..8u32 {
            assert_eq!(num(&with(&[(0x5012C, level << 16)]), "fgr"), level as i64, "level={level}");
        }
    }

    #[test]
    fn bgs_identity_patterns_mean_disabled() {
        assert!(!flag(&with(&[(0x50050, 0x8765_4321), (0x50058, 0x8765_4321)]), "bgs"));
        assert!(flag(&with(&[(0x50050, 0x8765_4321), (0x50058, 0x0123_4567)]), "bgs"));
    }

    #[test]
    fn bgs_is_unavailable_when_registers_are_missing() {
        let d = decoded();
        assert!(d.contains_key("bgs"));
        assert_eq!(d.get("bgs"), None);
    }

    #[test]
    fn real_granite_ridge_snapshot_matches_same_boot_oracle() {
        let regs: Registers = [
            (0x50200, 0x8001_1004), (0x50204, 0x082E_3A24),
            (0x50208, 0x002E_0068), (0x5020C, 0x0C00_0808),
            (0x50210, 0x0000_0020), (0x50214, 0x0012_0422),
            (0x50218, 0x0000_0030), (0x50220, 0x0801_0101),
            (0x50224, 0x0801_0101), (0x50228, 0x0000_1002),
            (0x50230, 0x00C0_FFFF), (0x50234, 0x2020_3A3A),
            (0x50250, 0x0007_0000), (0x50254, 0x0015_0D1F),
            (0x50258, 0x0623_1517), (0x50284, 0x0000_0131),
            (0x502A4, 0x0000_1314), (0x5012C, 0x0539_114A),
            (0x50260, 0x00C0_0138), (0x50264, 0x00C0_0138),
            (0x50268, 0x01E0_01E0), (0x5026C, 0x00C0_0138),
            (0x502C0, 0), (0x502C4, 0), (0x502C8, 0x007C_0186),
            (0x502CC, 0),
        ]
        .into_iter()
        .collect();
        let d = decode_channel(&regs).expect("decodes");
        let ints: &[(&str, i64)] = &[
            ("ratio", 4100), ("mclk_mhz", 4100), ("tCL", 36), ("tRCDRD", 46), ("tRCDWR", 8),
            ("tRAS", 58), ("tRP", 46), ("tRC", 104), ("tRTP", 12),
            ("tRRD_S", 8), ("tRRD_L", 8), ("tFAW", 32), ("tCWL", 34),
            ("tWTR_S", 4), ("tWTR_L", 18), ("tWR", 48),
            ("tRDRDSCL", 8), ("tRDRDSC", 1), ("tRDRDSD", 1),
            ("tRDRDDD", 1), ("tWRWRSCL", 8), ("tWRWRSC", 1),
            ("tWRWRSD", 1), ("tWRWRDD", 1), ("tRDWR", 16), ("tWRRD", 2),
            ("tREFI", 65535), ("tRFC", 480), ("tRFC2", 480),
            ("tRFCsb", 390), ("tMRD", 58), ("tMOD", 58),
            ("tMRDPDA", 32), ("tMODPDA", 32), ("tSTAG", 7),
            ("tSTAGsb", 0), ("tCKE", 0), ("tXP", 31), ("tPHYWRD", 6),
            ("tPHYRDL", 35), ("tPHYWRL", 21), ("tWRPOST", 1),
            ("tWRPRE", 4), ("tRDPOST", 1), ("tRDPRE", 4),
            ("nitro_rx", 1), ("nitro_tx", 3), ("nitro_ctrl", 1),
        ];
        for (name, expected) in ints {
            assert_eq!(d.get(name), Some(&UmcValue::Int(*expected)), "{name}");
        }
        assert_eq!(d.get("cmd_rate"), Some(&UmcValue::Text("1T".into())));
        assert_eq!(d.get("gdm"), Some(&UmcValue::Bool(false)));
        assert_eq!(d.get("powerdown"), Some(&UmcValue::Bool(false)));
    }

    #[test]
    fn nitro() {
        let d = decoded();
        assert_eq!(num(&d, "nitro_rx"), 1);
        assert_eq!(num(&d, "nitro_tx"), 3);
        assert_eq!(num(&d, "nitro_ctrl"), 1);
    }

    #[test]
    fn nitro_spec_bit_positions() {
        // 0x50284: Rx[9:8], Tx[5:4], Ctrl[1:0] -- asymmetric values so a
        // swapped Rx/Ctrl decode cannot pass by coincidence.
        let d = with(&[(0x50284, (2 << 8) | (1 << 4) | 3)]);
        assert_eq!(num(&d, "nitro_rx"), 2);
        assert_eq!(num(&d, "nitro_tx"), 1);
        assert_eq!(num(&d, "nitro_ctrl"), 3);
    }

    #[test]
    fn nitro_tx_is_two_bits() {
        // bit 6 lies outside Tx[5:4] and must not leak into the decode
        assert_eq!(num(&with(&[(0x50284, (1 << 6) | (3 << 4))]), "nitro_tx"), 3);
    }

    #[test]
    fn rfc_skips_placeholder_and_zero() {
        // place a valid value only after the placeholder/zero entries
        let d = with(&[(0x50260, 0x00C0_0138), (0x50264, (640 << 16) | 512)]);
        assert_eq!(num(&d, "tRFC"), 512);
        assert_eq!(num(&d, "tRFC2"), 640);
    }

    #[test]
    fn wrpre_rdpre_correction() {
        // WRPRE is always zero-based. RDPRE only adds one to encodings 0/1;
        // encodings >=2 are already literal.
        let d = with(&[(0x502A4, (3 << 8) | 2)]);
        assert_eq!(num(&d, "tWRPRE"), 4);
        assert_eq!(num(&d, "tRDPRE"), 2);
    }

    #[test]
    fn turnaround_field_widths() {
        let d = with(&[
            (0x50220, (0x3F << 24) | (0xF << 16) | (0xF << 8) | 0xF),
            (0x50224, (0x3F << 24) | (0xF << 16) | (0xF << 8) | 0xF),
            (0x50228, (0x3F << 8) | 0xF),
        ]);
        assert_eq!(num(&d, "tRDRDSC"), 0xF);
        assert_eq!(num(&d, "tWRWRSC"), 0xF);
        assert_eq!(num(&d, "tRDWR"), 0x3F);
    }

    // -- CcdlTest: tCCD_L and tCCD_L_WR2, both stored biased, outside the
    //    0x502xx block.

    #[test]
    fn bench_registers_decode_to_the_values_zentimings_reads() {
        let d = decoded();
        assert_eq!(num(&d, "tCCD_L"), 21);
        assert_eq!(num(&d, "tCCD_L_WR2"), 42);
    }

    #[test]
    fn the_bias_is_applied_to_the_field_not_the_register() {
        let d = with(&[(0x50198, 0x1B01_1300 | (10 << 3)), (0x502E0, 20)]); // field 10 -> 15, 20 -> 27
        assert_eq!(num(&d, "tCCD_L"), 15);
        assert_eq!(num(&d, "tCCD_L_WR2"), 27);
    }

    #[test]
    fn a_failed_read_is_not_shown_as_a_biased_number() {
        // All-ones would otherwise decode to a plausible 36 and 70.
        let d = with(&[(0x50198, 0xFFFF_FFFF), (0x502E0, 0xFFFF_FFFF)]);
        assert_eq!(d.get("tCCD_L"), None);
        assert_eq!(d.get("tCCD_L_WR2"), None);
    }

    #[test]
    fn a_missing_register_reports_nothing() {
        let mut regs = oracle_regs();
        regs.remove(&0x50198);
        regs.remove(&0x502E0);
        let d = decode_channel(&regs).expect("decodes");
        assert_eq!(d.get("tCCD_L"), None);
        assert_eq!(d.get("tCCD_L_WR2"), None);
    }

    #[test]
    fn values_below_the_controller_range_are_rejected() {
        let d = with(&[(0x50198, 0x1B01_1300), (0x502E0, 0)]); // 5 and 7, both below 8
        assert_eq!(d.get("tCCD_L"), None);
        assert_eq!(d.get("tCCD_L_WR2"), None);
    }

    // -- RejectTest ----------------------------------------------------------

    #[test]
    fn all_ones_rejected() {
        let regs: Registers =
            [0x50200, 0x50204, 0x50208, 0x5020C, 0x50214, 0x50230].into_iter().map(|off| (off, 0xFFFF_FFFF)).collect();
        assert!(decode_channel(&regs).is_none());
    }

    #[test]
    fn saturated_cl_rejected() {
        let mut regs = oracle_regs();
        regs.insert(0x50204, 0x3F | (58 << 8) | (46 << 16) | (8 << 24)); // CL saturated
        assert!(decode_channel(&regs).is_none());
    }

    #[test]
    fn zero_snapshot_rejected() {
        assert!(decode_channel(&Registers::new()).is_none());
    }

    #[test]
    fn implausible_literal_mclk_is_rejected() {
        let mut regs = oracle_regs();
        regs.insert(0x50200, 100);
        assert!(decode_channel(&regs).is_none());
    }

    // -- ChannelReadTest -----------------------------------------------------

    struct FakeReader {
        regs: Registers,
        base: u32,
    }

    impl SmnTransport for FakeReader {
        fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
            smn_addresses
                .iter()
                .map(|addr| (*addr, addr.checked_sub(self.base).and_then(|off| self.regs.get(&off).copied())))
                .collect()
        }

        fn last_error(&self) -> String {
            String::new()
        }
    }

    #[test]
    fn read_channel_uses_base_offset() {
        let base = 0x100000;
        let reader = FakeReader { regs: oracle_regs(), base };
        let d = read_channel(&reader, base).expect("decodes");
        assert_eq!(num(&d, "tCL"), 36);
    }

    #[test]
    fn bases() {
        assert_eq!(UMC_BASES, [0x0, 0x100000]);
    }

    // -- MiscSettingsTest (test_am5_misc_settings.py), the decode half ------

    fn misc(changes: &[(u32, u32)]) -> BTreeMap<&'static str, Option<String>> {
        decode_misc_settings(&changes.iter().copied().collect())
    }

    fn setting(values: &BTreeMap<&'static str, Option<String>>, name: &str) -> Option<String> {
        values.get(name).cloned().flatten()
    }

    #[test]
    fn register_changes_change_display() {
        let a = misc(&[(REG_PRE, 4 | (3 << 8) | (1 << 4) | (1 << 12)), (REG_DRAM_CONFIG, 0)]);
        assert_eq!(setting(&a, "read_preamble").as_deref(), Some("4 tCK - 00001010 Pattern"));
        assert_eq!(setting(&a, "write_preamble").as_deref(), Some("4 tCK - 00001010 Pattern"));
        assert_eq!(setting(&a, "read_postamble").as_deref(), Some("1.5 tCK - 010 Pattern"));
        assert_eq!(setting(&a, "write_postamble").as_deref(), Some("1.5 tCK - 000 Pattern"));
        assert_eq!(setting(&a, "ecc").as_deref(), Some("Disabled"));
        let b = misc(&[(REG_PRE, 0), (REG_DRAM_CONFIG, 1 << 12)]);
        assert_eq!(setting(&b, "read_preamble").as_deref(), Some("1 tCK - 10 Pattern"));
        assert_eq!(setting(&b, "write_postamble").as_deref(), Some("0.5 tCK - 0 Pattern"));
        assert_eq!(setting(&b, "ecc").as_deref(), Some("Enabled"));
    }

    #[test]
    fn failed_reads_are_not_disabled_or_default() {
        for regs in [misc(&[]), misc(&[(REG_PRE, 0xFFFF_FFFF), (REG_DRAM_CONFIG, 0xFFFF_FFFF)])] {
            assert_eq!(regs.len(), 5);
            assert!(regs.values().all(Option::is_none));
        }
    }

    #[test]
    fn reserved_postamble_is_not_a_guessed_duration() {
        for code in 2..8u32 {
            let values = misc(&[(REG_PRE, (code << 4) | (code << 12))]);
            let expected = format!("Reserved (UMC {code})");
            assert_eq!(setting(&values, "read_postamble"), Some(expected.clone()));
            assert_eq!(setting(&values, "write_postamble"), Some(expected));
        }
    }

    #[test]
    fn values_print_as_python_str_would() {
        assert_eq!(UmcValue::Int(36).to_string(), "36");
        assert_eq!(UmcValue::Float(125.0).to_string(), "125.0");
        assert_eq!(UmcValue::Bool(true).to_string(), "True");
        assert_eq!(UmcValue::Text("1T".into()).to_string(), "1T");
    }
}
