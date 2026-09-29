import unittest
from rochviewer.amd.timings import decode_misc_settings, REG_PRE, REG_DRAM_CONFIG
from rochviewer.amd.profile import (
    _brief_amble, _brief_read_preamble, _channel_setting_row,
)


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
        row = _channel_setting_row(
            "ECC", lambda channel: "Enabled" if channel == "cha" else "Disabled",
            "Other Settings", "Left")
        self.assertEqual(row["value"](), "A: Enabled | B: Disabled")
        self.assertEqual((row["value_a"](), row["value_b"]()),
                         ("Enabled", "Disabled"))
        self.assertEqual(row["Tab"], "Training")

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
        # One channel read: the shared value is that reading, and the other
        # channel's own value stays a dash rather than borrowing it.
        row = _channel_setting_row(
            "RX_DFE", lambda channel: 0 if channel == "cha" else "—",
            "Misc", "Right")
        self.assertEqual(row["value"](), 0)
        self.assertEqual(row["value_b"](), "—")


class BriefAmbleTest(unittest.TestCase):
    def test_a_length_alone_where_it_is_the_only_one(self):
        self.assertEqual(_brief_amble("4 tCK - 00001010 Pattern"), "4 tCK")
        self.assertEqual(_brief_amble("1.5 tCK - 010 Pattern"), "1.5 tCK")
        self.assertEqual(_brief_amble("0.5 tCK - 0 Pattern"), "0.5 tCK")
        # The write preamble has one 2 tCK setting, so no pattern.
        self.assertEqual(_brief_amble("2 tCK - 0010 Pattern"), "2 tCK")

    def test_the_read_preambles_two_2_tck_settings_stay_apart(self):
        self.assertEqual(_brief_read_preamble("2 tCK - 0010 Pattern"),
                         "2 tCK (0010)")
        self.assertEqual(_brief_read_preamble("2 tCK - 1110 Pattern"),
                         "2 tCK (1110)")
        self.assertEqual(_brief_read_preamble("4 tCK - 00001010 Pattern"),
                         "4 tCK")

    def test_anything_else_reads_as_decoded(self):
        self.assertEqual(_brief_amble("Reserved (UMC 5)"), "Reserved (UMC 5)")
        self.assertEqual(_brief_amble("\u2014"), "\u2014")
