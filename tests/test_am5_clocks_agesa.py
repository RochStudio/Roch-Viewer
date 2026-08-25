import struct
import unittest

from rochviewer.amd.agesa import find_agesa_version, parse_agesa_version
from rochviewer.amd.smu_clocks import (
    EXPECTED_TABLE_VERSION,
    check_cpu_gate,
    decode_clock_float,
    validate_clocks,
)


class AgesaParseTest(unittest.TestCase):
    def test_parse_agesa_v9_marker(self):
        blob = b"xxxxAGESA!V9ComboAm5PI 1.3.0.0\x00junk"
        self.assertEqual(parse_agesa_version(blob), "ComboAm5PI 1.3.0.0")

    def test_scan_chunks(self):
        payload = b"\x00" * 100 + b"AGESA!V9ComboAm5PI 1.2.0.3e" + b"\x00" * 50

        def reader(addr, size):
            if addr == 0x09000000:
                return payload
            return b"\x00" * size

        self.assertEqual(
            find_agesa_version(reader, start=0x09000000, end=0x09001000, chunk_size=256),
            "ComboAm5PI 1.2.0.3e",
        )


class SmuClockHelpersTest(unittest.TestCase):
    def test_cpu_gate_accepts_9850x3d(self):
        self.assertEqual(
            check_cpu_gate(cpu_name="AMD Ryzen 7 9850X3D 8-Core Processor"),
            "",
        )

    def test_cpu_gate_rejects_intel(self):
        self.assertTrue(
            check_cpu_gate(cpu_name="Intel(R) Core(TM) Ultra 7 270K Plus")
        )

    def test_decode_and_validate_zen_timings_shape(self):
        fclk = decode_clock_float(struct.unpack("<I", struct.pack("<f", 2000.0))[0])
        uclk = decode_clock_float(struct.unpack("<I", struct.pack("<f", 2050.0))[0])
        f, u = validate_clocks(fclk, uclk, umc_mclk=4100.0)
        self.assertEqual(f, 2000.0)
        self.assertEqual(u, 2050.0)

    def test_validate_rejects_nonsense(self):
        with self.assertRaises(ValueError):
            validate_clocks(50.0, 2050.0, umc_mclk=4100.0)

    def test_expected_version_constant(self):
        self.assertEqual(EXPECTED_TABLE_VERSION, 0x620105)


if __name__ == "__main__":
    unittest.main()
