import unittest
from dram_ic import identify_dram_ic

class DramIcTest(unittest.TestCase):
    def test_gskill_f5_6000_user_kit_is_hynix_a_die(self):
        self.assertEqual(identify_dram_ic("F5-6000J2636G16G"), "SK hynix A-die")
        self.assertEqual(identify_dram_ic("F5-6000J2636G16G", "G.Skill"), "SK hynix A-die")

    def test_unknown_stays_unknown(self):
        self.assertEqual(identify_dram_ic("COMPLETELY-FAKE-PART"), "Unknown IC")

if __name__ == "__main__":
    unittest.main()
