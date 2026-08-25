import unittest
from unittest import mock

from rochviewer.amd import adl as amd_adl


class VramVendorTest(unittest.TestCase):
    """Only the code this bench answered is named."""

    def test_the_bench_code_is_named(self):
        # ADL answers 1 for this card and GPU-Z, reading the same VBIOS its
        # own way, names the memory Samsung. Two tools, one card, agreeing.
        self.assertEqual(amd_adl.vram_vendor(1), "Samsung")

    def test_an_unmeasured_code_prints_as_itself(self):
        # The same rule the NVAPI tables follow. A list filled in from
        # documentation nobody checked here is how a table starts being
        # wrong quietly.
        self.assertEqual(amd_adl.vram_vendor(6), "Vendor 6")

    def test_nothing_read_is_not_a_vendor(self):
        self.assertIsNone(amd_adl.vram_vendor(None))


class ReadMemoryTest(unittest.TestCase):
    def test_a_machine_without_the_driver_says_nothing(self):
        # No Radeon driver, no atiadlxx.dll. Not an error: a machine this
        # module has nothing to say about.
        with mock.patch.object(amd_adl.ctypes, "CDLL",
                               side_effect=OSError("not found")):
            self.assertEqual(amd_adl.read_memory(), {})

    def test_a_library_that_will_not_start_says_nothing(self):
        library = mock.Mock()
        library.ADL2_Main_Control_Create.return_value = 1
        with mock.patch.object(amd_adl.ctypes, "CDLL", return_value=library):
            self.assertEqual(amd_adl.read_memory(), {})


if __name__ == "__main__":
    unittest.main()


class FormatSensorTest(unittest.TestCase):
    """ADL returns integers; the row names the unit."""

    def test_a_voltage_is_scaled_from_millivolts(self):
        self.assertEqual(amd_adl.format_sensor(749, "V", 0.001), "0.749 V")

    def test_a_fan_reads_as_whole_rpm(self):
        self.assertEqual(amd_adl.format_sensor(1825, "RPM", 1.0), "1825 RPM")

    def test_a_percentage_has_no_decimals(self):
        self.assertEqual(amd_adl.format_sensor(50, "%", 1.0), "50 %")

    def test_a_temperature_keeps_one(self):
        self.assertEqual(amd_adl.format_sensor(32, "\u00b0C", 1.0), "32.0 °C")


class PmlogSensorTest(unittest.TestCase):
    def test_the_four_exactly_matched_sensors_are_present(self):
        # Fan, fan PWM, utilisation and memory clock each matched HWiNFO to
        # the digit, which is what confirms the index enumeration rather than
        # merely making it plausible.
        indices = {index for index, _label, _unit, _scale
                   in amd_adl.PMLOG_SENSORS}
        for index in (14, 15, 19, 2):
            with self.subTest(index=index):
                self.assertIn(index, indices)

    def test_board_power_is_present_and_measured_in_watts(self):
        # Index 73 was written off as unreachable before it was ever printed.
        # A GPU load found it: 37 W at rest, pinned at 361-371 W under full
        # utilisation, back to 38 W after. A limit would not have moved.
        table = {index: (label, unit) for index, label, unit, _scale
                 in amd_adl.PMLOG_SENSORS}
        self.assertIn(73, table)
        self.assertEqual(table[73][1], "W")

    def test_watts_are_whole(self):
        # ADL hands over an integer; a decimal would invent precision.
        self.assertEqual(amd_adl.format_sensor(363, "W", 1.0), "363 W")

    def test_every_sensor_has_a_unit_the_formatter_knows(self):
        for _index, label, unit, scale in amd_adl.PMLOG_SENSORS:
            with self.subTest(label=label):
                self.assertIn(unit, ("V", "%", "RPM", "MHz", "W", "\u00b0C"))
                self.assertTrue(scale > 0)

    def test_a_machine_without_the_driver_reports_no_sensors(self):
        with mock.patch.object(amd_adl.ctypes, "CDLL",
                               side_effect=OSError("not found")):
            self.assertEqual(amd_adl.read_sensors(), {})
