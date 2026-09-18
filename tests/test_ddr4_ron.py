"""DDR4 MR1 decoding and source visibility, with no hardware access.

Synthetic register words verify decoding, not the board's shadow freshness
or whether ASUS firmware applies a requested RON setting.
"""

import unittest
from unittest.mock import patch

from rochviewer.platform_profiles import LGA1700_DDR4, LGA1700_DDR5, LGA1851
from tests.intel_stub import install, restore


class Ddr4RonTest(unittest.TestCase):
    def setUp(self):
        self.module = install(LGA1700_DDR4)
        self.addCleanup(restore)

    def reader(self, words):
        def read(address, size):
            self.assertEqual(size, 4)
            return words.get(address)
        return patch.object(self.module, "read_physical_memory_int", read)

    def test_both_standard_codes_and_reserved_codes(self):
        m = self.module
        # Low halfword is MR0: toggling its bits 2:1 must not select RON.
        for code, expected in ((0, "34 Ohm"), (1, "48 Ohm"),
                               (2, "Reserved (10b)"), (3, "Reserved (11b)")):
            for mr0 in (0x0000, 0x0006, 0xFFFF):
                with self.subTest(code=code, mr0=mr0):
                    mr1 = 0x0701 | (code << 1)  # RTT bits and DLL enable
                    raw = (mr1 << 16) | mr0
                    with self.reader({m.MCHBAR + o: raw for o in (0xE5A0, 0xF5A0)}):
                        self.assertEqual(m.get_ddr4_ron(m.MCHBAR), expected)

    def test_channel_sources_and_refresh_are_independent(self):
        m = self.module
        words = {m.MCHBAR + o: 0x00011234 for o in (0xE5A0, 0xF5A0)}
        words.update({m.CHANNEL_B + o: 0x00031234 for o in (0xE5A0, 0xF5A0)})
        row = next(r for r in m.TIMINGS if r.get("name") == "DRAM RON")
        with self.reader(words):
            self.assertEqual(row["value_a"](), "34 Ohm")
            self.assertEqual(row["value_b"](), "48 Ohm")
            words[m.MCHBAR + 0xE5A0] = 0x00031234
            self.assertEqual(row["value_a"](), "W0 48 Ohm / W1 34 Ohm")
            self.assertEqual(row["value_b"](), "48 Ohm")

    def test_unreadable_or_empty_windows_do_not_fabricate_34_or_hide_missing_peer(self):
        m = self.module
        for raw in (None, 0, 0xFFFFFFFF):
            with self.subTest(raw=raw), self.reader({
                m.MCHBAR + 0xE5A0: raw, m.MCHBAR + 0xF5A0: 0x00031234,
            }):
                self.assertEqual(m.get_ddr4_ron(m.MCHBAR), "W0 N/A / W1 48 Ohm")
            with self.reader({m.MCHBAR + o: raw for o in (0xE5A0, 0xF5A0)}):
                self.assertEqual(m.get_ddr4_ron(m.MCHBAR), "N/A")
        with patch.object(m, "read_physical_memory_int", side_effect=OSError("read failed")):
            self.assertEqual(m.get_ddr4_ron(m.MCHBAR), "N/A")

    def test_advanced_evidence_reports_the_same_full_word_and_mr1_field(self):
        m = self.module
        rows = [r for r in m.TIMINGS if r.get("advanced_only")]
        self.assertEqual(len(rows), 4)
        words = {m.MCHBAR + 0xE5A0: 0x00031234, m.MCHBAR + 0xF5A0: 0x00015678,
                 m.CHANNEL_B + 0xE5A0: 0x0003ABCD, m.CHANNEL_B + 0xF5A0: 0x0005ABCD}
        with self.reader(words):
            values = {r["name"]: (r["value_a"](), r["value_b"]()) for r in rows}
            self.assertEqual(values["DDR4 MR0/MR1 @E5A0"], ("0x00031234", "0x0003ABCD"))
            self.assertEqual(values["DDR4 MR1 ODI @F5A0"],
                             ("0x0001 (ODI 00)", "0x0005 (ODI 10)"))
        self.assertTrue(all(r["diagnostic"] for r in rows))

    def test_ddr4_has_one_ron_and_keeps_rtt_without_duplicate_odt(self):
        rows = [r for r in self.module.TIMINGS if r.get("Tab") == "Training"]
        self.assertEqual([r["name"] for r in rows if r.get("Category") == "RON"],
                         ["DRAM RON"])
        self.assertFalse(any(r.get("Category") == "ODT" for r in rows))
        self.assertEqual({r["name"] for r in rows if r.get("Category") == "RTT"},
                         {"RTT Wr", "RTT NOM", "RTT Park"})


class Ddr5UnchangedTest(unittest.TestCase):
    def test_ddr5_retains_two_drivers_and_six_odt_groups(self):
        for platform in (LGA1700_DDR5, LGA1851):
            m = install(platform)
            try:
                rows = [r for r in m.TIMINGS if r.get("Tab") == "Training"]
                self.assertEqual({r["name"] for r in rows if r.get("Category") == "RON"},
                                 {"Pull Up Drv", "Pull Down Drv"})
                self.assertEqual(len([r for r in rows if r.get("Category") == "ODT"]), 6)
                self.assertFalse(any(r.get("advanced_only") for r in m.TIMINGS))
            finally:
                restore()


if __name__ == "__main__":
    unittest.main()
