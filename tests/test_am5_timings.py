"""Decode tests for the AM5 UMC timing registers.

The tests synthesize register snapshots from an oracle set of timings,
feed them through the pure decoder, and assert the decoded values match.
No hardware is involved.
"""

import unittest

from rochviewer.amd import timings as t


def _oracle_regs():
    """Encode the oracle timing set into UMC register values."""
    regs = {}
    # 0x50200: ratio[15:0], Cmd2T bit17, GDM bit18
    regs[0x50200] = 4100                                   # MCLK 4100, 1T, GDM off
    # 0x50204: CL[5:0], RAS[14:8], RCDRD[21:16], RCDWR[29:24]
    regs[0x50204] = 36 | (58 << 8) | (46 << 16) | (8 << 24)
    # 0x50208: RP[21:16], RC[7:0]
    regs[0x50208] = 104 | (46 << 16)
    # 0x5020C: RTP[28:24], RRDL[12:8], RRDS[4:0]
    regs[0x5020C] = 8 | (8 << 8) | (12 << 24)
    # 0x50210: FAW[7:0]
    regs[0x50210] = 32
    # 0x50214: WTRL[22:16], WTRS[12:8], CWL[5:0]
    regs[0x50214] = 34 | (4 << 8) | (18 << 16)
    # 0x50218: WR[7:0]
    regs[0x50218] = 48
    # 0x50230: REFI[15:0]
    regs[0x50230] = 65535
    # tRFC / tRFC2: first nonzero, non-0x00C00138 in 0x50260/64/68/6C
    regs[0x50260] = 0x00C00138     # placeholder -> skipped
    regs[0x50264] = 0              # skipped
    regs[0x50268] = (480 << 16) | 480  # one active dword: tRFC2 | tRFC
    regs[0x5026C] = 0
    # tRFCsb: first nonzero low-11 in 0x502C0/C4/C8/CC
    regs[0x502C0] = 0
    regs[0x502C4] = 390
    # Nitro 0x50284: Rx=1, Tx=3, Ctrl=1
    regs[0x50284] = 1 | (3 << 4) | (1 << 8)
    # powerdown 0x5012C bit28
    regs[0x5012C] = 0
    # 0x50198: tCCD_L[7:3] + 5, and 0x502E0: tCCD_L_WR2[5:0] + 7.
    # Raw values captured on the bench, where ZenTimings reads 21 and 42.
    regs[0x50198] = 0x1B011385
    regs[0x502E0] = 0x00000023
    return regs


class DecodeTest(unittest.TestCase):
    def setUp(self):
        self.d = t.decode_channel(_oracle_regs())

    def test_snapshot_is_valid(self):
        self.assertIsNotNone(self.d)

    def test_primary_timings(self):
        d = self.d
        self.assertEqual(d["tCL"], 36)
        self.assertEqual(d["tRAS"], 58)
        self.assertEqual(d["tRCDRD"], 46)
        self.assertEqual(d["tRCDWR"], 8)
        self.assertEqual(d["tRP"], 46)
        self.assertEqual(d["tRC"], 104)

    def test_secondary_timings(self):
        d = self.d
        self.assertEqual(d["tRTP"], 12)
        self.assertEqual(d["tRRD_L"], 8)
        self.assertEqual(d["tRRD_S"], 8)
        self.assertEqual(d["tFAW"], 32)
        self.assertEqual(d["tCWL"], 34)
        self.assertEqual(d["tWTR_L"], 18)
        self.assertEqual(d["tWTR_S"], 4)
        self.assertEqual(d["tWR"], 48)

    def test_refresh_timings(self):
        d = self.d
        self.assertEqual(d["tREFI"], 65535)
        self.assertEqual(d["tRFC"], 480)
        self.assertEqual(d["tRFC2"], 480)
        self.assertEqual(d["tRFCsb"], 390)

    def test_trfc_nanoseconds_uses_active_refresh_timing(self):
        regs = _oracle_regs()
        regs[0x50200] = 4000
        regs[0x50268] = (600 << 16) | 500

        regs[0x5012C] = 0
        self.assertAlmostEqual(t.decode_channel(regs)["tRFC_ns"], 125.0)

        regs[0x5012C] = (1 << 16) | (1 << 1)
        self.assertAlmostEqual(t.decode_channel(regs)["tRFC_ns"], 150.0)

    def test_real_granite_ridge_trfc_nanoseconds(self):
        regs = _oracle_regs()
        regs[0x5012C] = 0x0539114A
        self.assertAlmostEqual(
            t.decode_channel(regs)["tRFC_ns"], 117.0731707317
        )

    def test_config_bits(self):
        d = self.d
        self.assertEqual(d["ratio"], 4100)
        self.assertEqual(d["mclk_mhz"], 4100)
        self.assertEqual(d["cmd_rate"], "1T")
        self.assertFalse(d["gdm"])
        self.assertFalse(d["powerdown"])

    def test_refresh_mode_decodes_fgr_and_per_bank_bits(self):
        cases = (
            (0, "Normal"),
            (1 << 16, "FGR"),
            (1 << 1, "Per-Bank Only"),
            ((1 << 16) | (1 << 1), "Mixed"),
        )
        for raw, expected in cases:
            with self.subTest(raw=raw):
                regs = _oracle_regs()
                regs[0x5012C] = raw
                decoded = t.decode_channel(regs)
                self.assertEqual(decoded["refresh_mode"], expected)

    def test_real_granite_ridge_refresh_mode_is_mixed(self):
        regs = _oracle_regs()
        regs[0x5012C] = 0x0539114A
        self.assertEqual(t.decode_channel(regs)["refresh_mode"], "Mixed")

    def test_fgr_is_reported_as_its_own_level(self):
        # "Mixed" cannot be read back to a setting: it says FGR is on and
        # per-bank is on, not which FGR level was chosen. This is that level,
        # and the same register the mode above comes from.
        regs = _oracle_regs()
        regs[0x5012C] = 0x0539114A
        decoded = t.decode_channel(regs)
        # ZenTimings shows 1 on this board, reading the same three bits.
        self.assertEqual(decoded["fgr"], 1)
        self.assertEqual(decoded["refresh_mode"], "Mixed")

    def test_fgr_reads_the_whole_three_bit_field(self):
        # A one-bit read would call level 4 zero and quietly say Normal.
        for level in range(8):
            with self.subTest(level=level):
                regs = _oracle_regs()
                regs[0x5012C] = level << 16
                self.assertEqual(t.decode_channel(regs)["fgr"], level)

    def test_bgs_identity_patterns_mean_disabled(self):
        regs = _oracle_regs()
        regs[0x50050] = 0x87654321
        regs[0x50058] = 0x87654321
        self.assertFalse(t.decode_channel(regs)["bgs"])
        regs[0x50058] = 0x01234567
        self.assertTrue(t.decode_channel(regs)["bgs"])

    def test_bgs_is_unavailable_when_registers_are_missing(self):
        self.assertIsNone(self.d["bgs"])

    def test_real_granite_ridge_snapshot_matches_same_boot_oracle(self):
        regs = {
            0x50200: 0x80011004, 0x50204: 0x082E3A24,
            0x50208: 0x002E0068, 0x5020C: 0x0C000808,
            0x50210: 0x00000020, 0x50214: 0x00120422,
            0x50218: 0x00000030, 0x50220: 0x08010101,
            0x50224: 0x08010101, 0x50228: 0x00001002,
            0x50230: 0x00C0FFFF, 0x50234: 0x20203A3A,
            0x50250: 0x00070000, 0x50254: 0x00150D1F,
            0x50258: 0x06231517, 0x50284: 0x00000131,
            0x502A4: 0x00001314, 0x5012C: 0x0539114A,
            0x50260: 0x00C00138, 0x50264: 0x00C00138,
            0x50268: 0x01E001E0, 0x5026C: 0x00C00138,
            0x502C0: 0, 0x502C4: 0, 0x502C8: 0x007C0186,
            0x502CC: 0,
        }
        d = t.decode_channel(regs)
        expected = {
            "ratio": 4100, "mclk_mhz": 4100, "cmd_rate": "1T",
            "gdm": False, "tCL": 36, "tRCDRD": 46, "tRCDWR": 8,
            "tRAS": 58, "tRP": 46, "tRC": 104, "tRTP": 12,
            "tRRD_S": 8, "tRRD_L": 8, "tFAW": 32, "tCWL": 34,
            "tWTR_S": 4, "tWTR_L": 18, "tWR": 48,
            "tRDRDSCL": 8, "tRDRDSC": 1, "tRDRDSD": 1,
            "tRDRDDD": 1, "tWRWRSCL": 8, "tWRWRSC": 1,
            "tWRWRSD": 1, "tWRWRDD": 1, "tRDWR": 16, "tWRRD": 2,
            "tREFI": 65535, "tRFC": 480, "tRFC2": 480,
            "tRFCsb": 390, "tMRD": 58, "tMOD": 58,
            "tMRDPDA": 32, "tMODPDA": 32, "tSTAG": 7,
            "tSTAGsb": 0, "tCKE": 0, "tXP": 31, "tPHYWRD": 6,
            "tPHYRDL": 35, "tPHYWRL": 21, "tWRPOST": 1,
            "tWRPRE": 4, "tRDPOST": 1, "tRDPRE": 4,
            "nitro_rx": 1, "nitro_tx": 3, "nitro_ctrl": 1,
            "powerdown": False,
        }
        self.assertEqual({key: d[key] for key in expected}, expected)

    def test_nitro(self):
        d = self.d
        self.assertEqual(d["nitro_rx"], 1)
        self.assertEqual(d["nitro_tx"], 3)
        self.assertEqual(d["nitro_ctrl"], 1)

    def test_nitro_spec_bit_positions(self):
        # 0x50284: Rx[9:8], Tx[5:4], Ctrl[1:0] -- asymmetric values so a
        # swapped Rx/Ctrl decode cannot pass by coincidence.
        regs = _oracle_regs()
        regs[0x50284] = (2 << 8) | (1 << 4) | 3
        d = t.decode_channel(regs)
        self.assertEqual(d["nitro_rx"], 2)
        self.assertEqual(d["nitro_tx"], 1)
        self.assertEqual(d["nitro_ctrl"], 3)

    def test_nitro_tx_is_two_bits(self):
        # bit 6 lies outside Tx[5:4] and must not leak into the decode
        regs = _oracle_regs()
        regs[0x50284] = (1 << 6) | (3 << 4)
        self.assertEqual(t.decode_channel(regs)["nitro_tx"], 3)

    def test_rfc_skips_placeholder_and_zero(self):
        regs = _oracle_regs()
        # place a valid value only after the placeholder/zero entries
        regs[0x50260] = 0x00C00138
        regs[0x50264] = (640 << 16) | 512
        self.assertEqual(t.decode_channel(regs)["tRFC"], 512)
        self.assertEqual(t.decode_channel(regs)["tRFC2"], 640)

    def test_wrpre_rdpre_correction(self):
        regs = _oracle_regs()
        # WRPRE is always zero-based. RDPRE only adds one to encodings 0/1;
        # encodings >=2 are already literal.
        regs[0x502A4] = (3 << 8) | 2
        d = t.decode_channel(regs)
        self.assertEqual(d["tWRPRE"], 4)
        self.assertEqual(d["tRDPRE"], 2)

    def test_turnaround_field_widths(self):
        regs = _oracle_regs()
        regs[0x50220] = (0x3F << 24) | (0xF << 16) | (0xF << 8) | 0xF
        regs[0x50224] = (0x3F << 24) | (0xF << 16) | (0xF << 8) | 0xF
        regs[0x50228] = (0x3F << 8) | 0xF
        d = t.decode_channel(regs)
        self.assertEqual(d["tRDRDSC"], 0xF)
        self.assertEqual(d["tWRWRSC"], 0xF)
        self.assertEqual(d["tRDWR"], 0x3F)


class CcdlTest(unittest.TestCase):
    """tCCD_L and tCCD_L_WR2, both stored biased, outside the 0x502xx block."""

    def test_bench_registers_decode_to_the_values_zentimings_reads(self):
        d = t.decode_channel(_oracle_regs())
        self.assertEqual(d["tCCD_L"], 21)
        self.assertEqual(d["tCCD_L_WR2"], 42)

    def test_the_bias_is_applied_to_the_field_not_the_register(self):
        regs = _oracle_regs()
        regs[0x50198] = 0x1B011300 | (10 << 3)      # field 10 -> 15
        regs[0x502E0] = 20                          # field 20 -> 27
        d = t.decode_channel(regs)
        self.assertEqual(d["tCCD_L"], 15)
        self.assertEqual(d["tCCD_L_WR2"], 27)

    def test_a_failed_read_is_not_shown_as_a_biased_number(self):
        # All-ones would otherwise decode to a plausible 36 and 70.
        regs = _oracle_regs()
        regs[0x50198] = 0xFFFFFFFF
        regs[0x502E0] = 0xFFFFFFFF
        d = t.decode_channel(regs)
        self.assertIsNone(d["tCCD_L"])
        self.assertIsNone(d["tCCD_L_WR2"])

    def test_a_missing_register_reports_nothing(self):
        regs = _oracle_regs()
        del regs[0x50198]
        del regs[0x502E0]
        d = t.decode_channel(regs)
        self.assertIsNone(d["tCCD_L"])
        self.assertIsNone(d["tCCD_L_WR2"])

    def test_values_below_the_controller_range_are_rejected(self):
        regs = _oracle_regs()
        regs[0x50198] = 0x1B011300                  # field 0 -> 5, below 8
        regs[0x502E0] = 0                           # field 0 -> 7, below 8
        d = t.decode_channel(regs)
        self.assertIsNone(d["tCCD_L"])
        self.assertIsNone(d["tCCD_L_WR2"])


class RejectTest(unittest.TestCase):
    def test_all_ones_rejected(self):
        regs = {off: 0xFFFFFFFF for off in (
            0x50200, 0x50204, 0x50208, 0x5020C, 0x50214, 0x50230)}
        self.assertIsNone(t.decode_channel(regs))

    def test_saturated_cl_rejected(self):
        regs = _oracle_regs()
        regs[0x50204] = 0x3F | (58 << 8) | (46 << 16) | (8 << 24)  # CL saturated
        self.assertIsNone(t.decode_channel(regs))

    def test_zero_snapshot_rejected(self):
        self.assertIsNone(t.decode_channel({}))

    def test_implausible_literal_mclk_is_rejected(self):
        regs = _oracle_regs()
        regs[0x50200] = 100
        self.assertIsNone(t.decode_channel(regs))


class ChannelReadTest(unittest.TestCase):
    def test_read_channel_uses_base_offset(self):
        regs = _oracle_regs()
        base = 0x100000

        class FakeReader:
            def read(self, addr):
                return regs.get(addr - base)

        d = t.read_channel(FakeReader(), base)
        self.assertEqual(d["tCL"], 36)

    def test_bases(self):
        self.assertEqual(t.UMC_BASES, (0x0, 0x100000))


if __name__ == "__main__":
    unittest.main()
