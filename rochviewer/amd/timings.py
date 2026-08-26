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

"""Decoder for AMD AM5 (Zen 4 / Granite Ridge) UMC timing registers.

The UMC register block for each channel lives at an SMN base of 0x0 and
0x100000; every timing register offset below is relative to that base.

``decode_channel`` is a pure function over a ``{offset: value}`` snapshot so
it can be unit-tested without any hardware access.  ``read_channel`` pulls a
live snapshot through an :class:`amd_smn.SmnReader`-compatible object and
decodes it, returning ``None`` for implausible / failed snapshots.
"""

# UMC channel SMN bases.
UMC_BASES = (0x0, 0x100000)

# Register offsets (relative to a channel base).
REG_BGS0 = 0x50050        # bank-group-swap pattern register 0
REG_BGS1 = 0x50058        # bank-group-swap pattern register 1
REG_BGSA0 = 0x500D0       # bank-group-swap-alt register 0
REG_BGSA1 = 0x500D4       # bank-group-swap-alt register 1
REG_CONFIG = 0x50200      # ratio[15:0], Cmd2T bit17, GDM bit18
REG_RCD = 0x50204         # CL[5:0], RAS[14:8], RCDRD[21:16], RCDWR[29:24]
REG_RP_RC = 0x50208       # RP[21:16], RC[7:0]
REG_RTP_RRD = 0x5020C     # RTP[28:24], RRDL[12:8], RRDS[4:0]
REG_FAW = 0x50210         # FAW[7:0]
REG_WTR_CWL = 0x50214     # WTRL[22:16], WTRS[12:8], CWL[5:0]
REG_WR = 0x50218          # WR[7:0]
REG_RDRD = 0x50220        # tertiary read-to-read
REG_WRWR = 0x50224        # tertiary write-to-write
REG_RDWR = 0x50228        # read<->write turnaround
REG_REFI = 0x50230        # REFI[15:0]
REG_MOD = 0x50234         # MODPDA[29:24], MRDPDA[21:16], MOD[13:8], MRD[5:0]
REG_STAG = 0x50250        # STAG[26:16], STAGsb[8:0]
REG_CKE_XP = 0x50254      # CKE[28:24], XP[5:0]
REG_PHY = 0x50258         # PHYWRD[26:24], PHYRDL[23:16], PHYWRL[15:8]
REG_NITRO = 0x50284       # Nitro dials: Rx[9:8], Tx[5:4], Ctrl[1:0]
REG_PRE = 0x502A4         # WRPOST/WRPRE/RDPOST/RDPRE (+1 correction on PRE)
REG_PD = 0x5012C          # powerdown enable bit28
REG_CCDL = 0x50198        # tCCD_L[7:3] + 5
REG_CCDL_WR2 = 0x502E0    # tCCD_L_WR2[5:0] + 7

# Both tCCD_L registers store a biased value, so an all-ones (failed) read
# decodes to a plausible-looking number rather than an obvious one. These are
# the ranges outside which the decode is rejected instead of displayed; they
# match the bounds ZenTimings applies to the same two registers.
CCDL_RANGE = (8, 36)
CCDL_WR2_RANGE = (8, 70)

# tRFC / tRFC2 candidates; the first active dword contains both values.
RFC_OFFSETS = (0x50260, 0x50264, 0x50268, 0x5026C)
RFC_PLACEHOLDER = 0x00C00138

# tRFCsb candidates; first with nonzero low-11 wins.
RFCSB_OFFSETS = (0x502C0, 0x502C4, 0x502C8, 0x502CC)

# Every offset read for a live snapshot.
ALL_OFFSETS = (
    REG_BGS0, REG_BGS1, REG_BGSA0, REG_BGSA1,
    REG_CONFIG, REG_RCD, REG_RP_RC, REG_RTP_RRD,
    REG_FAW, REG_WTR_CWL,
    REG_WR, REG_RDRD, REG_WRWR, REG_RDWR, REG_REFI, REG_MOD, REG_STAG,
    REG_CKE_XP, REG_PHY, REG_NITRO, REG_PRE, REG_PD,
    REG_CCDL, REG_CCDL_WR2,
) + RFC_OFFSETS + RFCSB_OFFSETS

_INVALID = 0xFFFFFFFF
_BGS_DISABLED_PATTERN = 0x87654321


def _bits(value, low, width):
    return (value >> low) & ((1 << width) - 1)


def _biased_field(regs, offset, low, width, bias, valid_range):
    """Decode a biased tCCD_L field, or None when it is out of range.

    Missing register, failed read or a value outside the range the controller
    can hold all mean the same thing to a reader: this is not a timing. None
    surfaces as an em-dash rather than as a number that looks real.
    """
    raw = regs.get(offset)
    if raw is None or raw == _INVALID:
        return None
    value = _bits(raw, low, width) + bias
    low_bound, high_bound = valid_range
    if not low_bound <= value <= high_bound:
        return None
    return value


def _first_rfc_pair(regs, offsets):
    for off in offsets:
        val = regs.get(off, 0)
        if val != 0 and val != RFC_PLACEHOLDER:
            return val & 0xFFFF, (val >> 16) & 0xFFFF
    return 0, 0


def _first_rfcsb(regs, offsets):
    for off in offsets:
        low = regs.get(off, 0) & 0x7FF
        if low != 0:
            return low
    return 0


def _decode_bgs_alt(regs):
    """Bank-group-swap-alt, a separate control from BGS.

    Enabled when either alt register has a nonzero field at bits 4..10; the
    pattern comparison used for BGS does not apply here.  Register numbers and
    the bit field follow the ZenStates-Core BaseDramTimings reference.
    """
    if REG_BGSA0 not in regs or REG_BGSA1 not in regs:
        return None
    if regs[REG_BGSA0] == _INVALID or regs[REG_BGSA1] == _INVALID:
        return None
    return bool(
        _bits(regs[REG_BGSA0], 4, 7) or _bits(regs[REG_BGSA1], 4, 7)
    )


def _decode_bgs(regs):
    if REG_BGS0 not in regs or REG_BGS1 not in regs:
        return None
    if regs[REG_BGS0] == _INVALID or regs[REG_BGS1] == _INVALID:
        return None
    return not (
        regs[REG_BGS0] == _BGS_DISABLED_PATTERN
        and regs[REG_BGS1] == _BGS_DISABLED_PATTERN
    )


def _decode_refresh_mode(value):
    fgr = _bits(value, 16, 3)
    per_bank = bool(_bits(value, 1, 1))
    if per_bank:
        return "Mixed" if fgr else "Per-Bank Only"
    return "FGR" if fgr else "Normal"


def _is_plausible(regs):
    """Reject 0xFFFFFFFF / saturated / empty snapshots."""
    rcd = regs.get(REG_RCD, 0)
    config = regs.get(REG_CONFIG, 0)
    mclk_mhz = _bits(config, 0, 16)
    if not 1000 <= mclk_mhz <= 6000:
        return False
    if rcd == 0 or rcd == _INVALID:
        return False
    cl = _bits(rcd, 0, 6)
    if cl == 0 or cl >= 0x3F:            # 0 or saturated
        return False
    ras = _bits(rcd, 8, 7)
    if ras == 0 or ras >= 0x7F:
        return False
    for off in (REG_RP_RC, REG_RTP_RRD, REG_WTR_CWL):
        if regs.get(off, 0) == _INVALID:
            return False
    return True


def decode_channel(regs):
    """Decode a ``{offset: value}`` snapshot into named timings.

    Returns ``None`` if the snapshot is missing, saturated or otherwise
    implausible.
    """
    if not _is_plausible(regs):
        return None

    config = regs.get(REG_CONFIG, 0)
    rcd = regs.get(REG_RCD, 0)
    rp_rc = regs.get(REG_RP_RC, 0)
    rtp = regs.get(REG_RTP_RRD, 0)
    wtr = regs.get(REG_WTR_CWL, 0)
    mod = regs.get(REG_MOD, 0)
    stag = regs.get(REG_STAG, 0)
    cke = regs.get(REG_CKE_XP, 0)
    phy = regs.get(REG_PHY, 0)
    rdrd = regs.get(REG_RDRD, 0)
    wrwr = regs.get(REG_WRWR, 0)
    rdwr = regs.get(REG_RDWR, 0)
    nitro = regs.get(REG_NITRO, 0)
    pre = regs.get(REG_PRE, 0)
    pd = regs.get(REG_PD, 0)

    ratio = _bits(config, 0, 16)

    rfc, rfc2 = _first_rfc_pair(regs, RFC_OFFSETS)
    rfcsb = _first_rfcsb(regs, RFCSB_OFFSETS)
    refresh_mode = _decode_refresh_mode(pd)
    # Outside Normal mode the controller refreshes on tRFC2, so tRFC itself is
    # not the interval in effect.
    active_rfc = rfc if refresh_mode == "Normal" else rfc2

    def _to_ns(cycles):
        return cycles * 1000.0 / ratio if cycles and ratio else None

    rfc_ns = _to_ns(active_rfc)
    rfcsb_ns = _to_ns(rfcsb)
    rdpre_raw = _bits(pre, 0, 3)

    decoded = {
        "ratio": ratio,
        "mclk_mhz": ratio,
        "cmd_rate": "2T" if _bits(config, 17, 1) else "1T",
        "gdm": bool(_bits(config, 18, 1)),
        "bgs": _decode_bgs(regs),
        "bgs_alt": _decode_bgs_alt(regs),

        "tCL": _bits(rcd, 0, 6),
        "tRAS": _bits(rcd, 8, 7),
        "tRCDRD": _bits(rcd, 16, 6),
        "tRCDWR": _bits(rcd, 24, 6),

        "tRP": _bits(rp_rc, 16, 6),
        "tRC": _bits(rp_rc, 0, 8),

        "tRTP": _bits(rtp, 24, 5),
        "tRRD_L": _bits(rtp, 8, 5),
        "tRRD_S": _bits(rtp, 0, 5),

        "tFAW": _bits(regs.get(REG_FAW, 0), 0, 8),

        "tCWL": _bits(wtr, 0, 6),
        "tWTR_S": _bits(wtr, 8, 5),
        "tWTR_L": _bits(wtr, 16, 7),

        "tWR": _bits(regs.get(REG_WR, 0), 0, 8),

        # tertiary / turnaround
        "tRDRDSCL": _bits(rdrd, 24, 6),
        "tRDRDSC": _bits(rdrd, 16, 4),
        "tRDRDSD": _bits(rdrd, 8, 4),
        "tRDRDDD": _bits(rdrd, 0, 4),
        "tWRWRSCL": _bits(wrwr, 24, 6),
        "tWRWRSC": _bits(wrwr, 16, 4),
        "tWRWRSD": _bits(wrwr, 8, 4),
        "tWRWRDD": _bits(wrwr, 0, 4),
        "tRDWR": _bits(rdwr, 8, 6),
        "tWRRD": _bits(rdwr, 0, 4),

        # tCCD_L sits outside the 0x502xx timing block and is stored biased.
        # tCCD_L_WR is in neither register; see amd_apob.find_ccdl_wr.
        "tCCD_L": _biased_field(regs, REG_CCDL, 3, 5, 5, CCDL_RANGE),
        "tCCD_L_WR2": _biased_field(
            regs, REG_CCDL_WR2, 0, 6, 7, CCDL_WR2_RANGE
        ),

        "tREFI": _bits(regs.get(REG_REFI, 0), 0, 16),
        "tRFC": rfc,
        "tRFC2": rfc2,
        "tRFCsb": rfcsb,
        "tRFCsb_ns": rfcsb_ns,
        "tRFC_ns": rfc_ns,
        "refresh_mode": refresh_mode,
        # The raw fine-granularity-refresh field, which refresh_mode above
        # folds into a word along with the per-bank bit. Both are reported
        # because they answer different questions: the mode says what the
        # controller is doing, this says which FGR level it was set to, and
        # "Mixed" alone cannot be read back to a setting.
        "fgr": _bits(pd, 16, 3),

        "tMRD": _bits(mod, 0, 6),
        "tMOD": _bits(mod, 8, 6),
        "tMRDPDA": _bits(mod, 16, 6),
        "tMODPDA": _bits(mod, 24, 6),

        "tSTAG": _bits(stag, 16, 11),
        "tSTAGsb": _bits(stag, 0, 9),
        "tCKE": _bits(cke, 24, 5),
        "tXP": _bits(cke, 0, 6),
        "tPHYWRD": _bits(phy, 24, 3),
        "tPHYRDL": _bits(phy, 16, 8),
        "tPHYWRL": _bits(phy, 8, 8),

        # AMD encodes WRPRE zero-based. RDPRE encodings 0/1 are zero-based;
        # values >=2 are already literal in the validated DDR5 UMC layout.
        "tWRPOST": _bits(pre, 12, 3),
        "tWRPRE": _bits(pre, 8, 3) + 1,
        "tRDPOST": _bits(pre, 4, 3),
        "tRDPRE": rdpre_raw + 1 if rdpre_raw < 2 else rdpre_raw,

        "nitro_rx": _bits(nitro, 8, 2),
        "nitro_tx": _bits(nitro, 4, 2),
        "nitro_ctrl": _bits(nitro, 0, 2),

        "powerdown": bool(_bits(pd, 28, 1)),
    }
    return decoded


def read_channel(reader, base):
    """Read and decode one UMC channel through ``reader``.

    ``reader.read(smn_address)`` must return the 32-bit value or ``None``.
    Returns the decoded dict, or ``None`` for a failed / implausible read.
    """
    regs = {}
    for off in ALL_OFFSETS:
        val = reader.read(base + off)
        if val is not None:
            regs[off] = val & 0xFFFFFFFF
    return decode_channel(regs)

