# Roch Viewer -- a read-only memory-controller and timing viewer.

import unittest
from unittest import mock

from rochviewer.memory.ddr4_spd import read_modules as read_ddr4_modules
from rochviewer.memory.spd_profiles import (
    _join_slots,
    decode_ddr4_base,
    decode_ddr4_spd,
    decode_ddr4_xmp,
)


def sample_spd():
    values = {offset: 0 for offset in range(487)}
    values.update({
        2: 0x0C,       # DDR4
        3: 0x02,       # UDIMM
        4: 0x05,       # 8 Gbit SDRAM
        12: 0x09,      # 2 ranks, x8 devices
        13: 0x03,      # 64-bit bus
        18: 0x08,
        20: 0x80,      # CL14
        21: 0x03,      # CL15, CL16
        24: 0x6C,      # 13.5 ns tAA
        25: 0x70,
        26: 0x70,
        27: 0x11,
        28: 0x20,
        29: 0x90,
        125: 0xC2,     # 8*125-62 = 938 ps, DDR4-2133
        384: 0x0C,
        385: 0x4A,
        386: 0x01,
        387: 0x20,
        393: 0xA3,     # 1.35 V
        396: 0x05,     # 625 ps, DDR4-3200
        397: 0x80,     # CL14 supported
        401: 0x46,     # 8.75 ns => CL14
        402: 0x46,
        403: 0x46,
        404: 0x00,
        405: 0xAA,     # 21.25 ns => 34 clocks
        406: 0xF0,     # 30 ns => 48 clocks
    })
    return values


class Ddr4SpdProfileTest(unittest.TestCase):
    def test_base_module_geometry_and_bandwidth(self):
        decoded = decode_ddr4_base(sample_spd())
        self.assertEqual(decoded["module_type"], "UDIMM")
        self.assertEqual(decoded["capacity"], "16 GB")
        self.assertEqual(decoded["rank"], "2R")
        self.assertEqual(decoded["max_bandwidth"], "DDR4-2133 (1066 MHz)")
        self.assertEqual([item["cl"] for item in decoded["profiles"]],
                         [14, 15, 16])

    def test_xmp_20_profile(self):
        decoded = decode_ddr4_xmp(sample_spd())
        self.assertEqual(decoded["extension"], "XMP 2.0")
        self.assertEqual(len(decoded["profiles"]), 1)
        profile = decoded["profiles"][0]
        self.assertEqual(profile["name"], "XMP-3200")
        self.assertEqual(profile["frequency"], "1600 MHz")
        self.assertEqual(
            [profile[key] for key in ("cl", "trcd", "trp", "tras", "trc")],
            [14, 14, 14, 34, 48],
        )
        self.assertEqual(profile["voltage"], "1.35 V")

    def test_identity_and_profiles_are_combined(self):
        decoded = decode_ddr4_spd(sample_spd(), {"part_number": "TEST-3200"})
        self.assertEqual(decoded["part_number"], "TEST-3200")
        self.assertEqual(len(decoded["profiles"]), 4)

    def test_non_ddr4_data_is_rejected(self):
        values = sample_spd()
        values[2] = 0x12
        self.assertIsNone(decode_ddr4_spd(values))

    def test_slot_join_does_not_infer_dram_identity_from_part_number(self):
        module = {
            "part_number": "KNOWN-KIT",
            "serial_number": "1",
            "dram_manufacturer": "—",
            "dram_die": "—",
        }
        inventory = [{
            "part_number": "KNOWN-KIT",
            "serial_number": "1",
            "slot": "A2",
            "ic": "Samsung B-die",
        }]
        joined = _join_slots([module], inventory)[0]
        self.assertEqual(joined["slot"], "A2")
        self.assertEqual(joined["dram_manufacturer"], "—")
        self.assertEqual(joined["dram_die"], "—")

    def test_reader_skips_reserved_spd_ranges(self):
        class Reader:
            def __init__(self):
                self.ranges = []

            def is_driver_open(self):
                return True

            def read_ddr4_spd(self, address, offset, length,
                              controller_offset=0):
                self.ranges.append((offset, length))
                source = sample_spd()
                return {
                    position: source.get(position)
                    for position in range(offset, offset + length)
                }

        reader = Reader()
        identity = [{
            "address": 0x51,
            "controller": 0,
            "part_number": "TEST-3200",
        }]
        with mock.patch(
            "rochviewer.memory.ddr5_telemetry.default_smbus_backend",
            return_value=None,
        ), mock.patch(
            "rochviewer.memory.ddr4_spd.read_identity", return_value=identity
        ):
            found = read_ddr4_modules(reader_factory=lambda: reader)
        self.assertEqual(len(found), 1)
        self.assertEqual(reader.ranges, [
            (2, 28), (120, 6), (384, 6), (393, 14), (427, 5),
        ])


if __name__ == "__main__":
    unittest.main()
