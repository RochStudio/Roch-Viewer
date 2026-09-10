import unittest
from rochviewer.amd.timings import decode_misc_settings, REG_PRE, REG_DRAM_CONFIG
from rochviewer.amd.profile import _misc_channels


class MiscSettingsTest(unittest.TestCase):
    def test_register_changes_change_display(self):
        a = decode_misc_settings({REG_PRE: 4 | (3 << 8) | (1 << 4) | (1 << 12), REG_DRAM_CONFIG: 0})
        self.assertEqual(a["read_preamble"], "4 tCK - 00001010 Pattern")
        self.assertEqual(a["write_preamble"], "4 tCK - 00001010 Pattern")
        self.assertEqual(a["read_postamble"], "1.5 tCK - 010 Pattern")
        self.assertEqual(a["write_postamble"], "1.5 tCK - 000 Pattern")
        self.assertEqual(a["ecc"], "Disabled")
        b = decode_misc_settings({REG_PRE: 0, REG_DRAM_CONFIG: 1 << 12})
        self.assertEqual(b["read_preamble"], "1 tCK - 10 Pattern")
        self.assertEqual(b["write_postamble"], "0.5 tCK - 0 Pattern")
        self.assertEqual(b["ecc"], "Enabled")

    def test_failed_reads_are_not_disabled_or_default(self):
        for regs in ({}, {REG_PRE: 0xFFFFFFFF, REG_DRAM_CONFIG: 0xFFFFFFFF}):
            self.assertTrue(all(v is None for v in decode_misc_settings(regs).values()))

    def test_channels_are_not_mirrored(self):
        class Runtime:
            def channel_umc_value(self, name, channel):
                return "Enabled" if channel == "cha" else "Disabled"
        self.assertEqual(_misc_channels(Runtime(), "ecc")(), "A: Enabled | B: Disabled")

    def test_reserved_postamble_is_not_a_guessed_duration(self):
        for code in range(2, 8):
            values = decode_misc_settings({REG_PRE: code << 4 | code << 12})
            self.assertEqual(values["read_postamble"], f"Reserved (UMC {code})")
            self.assertEqual(values["write_postamble"], f"Reserved (UMC {code})")

    def test_raw_training_codes_follow_bytes_and_preserve_known_fields(self):
        from rochviewer.amd.apob import decode_granite_ridge_training_block
        block = bytearray(0x30)
        block[0x10:0x17] = bytes((9, 40, 60, 30, 91, 82, 43))
        values = decode_granite_ridge_training_block(block)
        self.assertEqual([values[n] for n in ("ALERT_PU", "CA_DRV", "PHY_VREF",
                          "DQ_VREF", "CA_VREF", "CS_VREF", "RX_DFE", "TX_DFE")],
                         [9, 9, 40, 60, 30, 91, 82, 43])
        self.assertEqual(values["proc_ca_ds"], "40 Ω")
        block[0x14] = 0
        self.assertEqual(decode_granite_ridge_training_block(block)["CS_VREF"], 0)
        self.assertIsNone(decode_granite_ridge_training_block(block[:0x16]))

    def test_raw_training_channels_keep_missing_channel_blank(self):
        class Runtime:
            def channel_training_value(self, name, channel):
                return 0 if channel == "cha" else "—"
        self.assertEqual(_misc_channels(Runtime(), "RX_DFE", training=True)(),
                         "A: 0 | B: —")
