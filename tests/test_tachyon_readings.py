import unittest
from unittest.mock import Mock, patch

from rochviewer.memory.dimm_inventory import parse_slot
from rochviewer.sensors.am5_board_rails import read_board_rails
from rochviewer.ui.main import TimingGUI


class TachyonReadingsTest(unittest.TestCase):
    def test_split_channel_and_dimm_labels(self):
        self.assertEqual(parse_slot("DIMM 1", "P0 CHANNEL A"), "A1")
        self.assertEqual(parse_slot("DIMM 1", "P0 CHANNEL B"), "B1")
        self.assertIsNone(parse_slot("DIMM 1", "Unknown"))

    def test_native_vddio(self):
        reader = Mock()
        reader.read_voltage.return_value = 1.416
        identity = ("GIGABYTE TECHNOLOGY CO., LTD.", "X870 AORUS TACHYON ICE")
        self.assertEqual(read_board_rails(identity, reader), {"vddio_mem": 1.416})
        reader.read_voltage.assert_called_once_with("IT8696E", 0x26)
        for invalid in (None, 0, 3.3, float("nan")):
            reader.read_voltage.return_value = invalid
            self.assertEqual(read_board_rails(identity, reader), {})

    def test_graphics_not_polled_by_telemetry(self):
        from rochviewer.ui import main
        tab = next(iter(main.WINDOWED_TABS))
        rows = [{"Tab": tab, "Category": "Graphics", "name": "GPU Clock"},
                {"Tab": tab, "Category": "Voltages", "name": "VDDIO"}]
        with patch.object(main, "TIMINGS", rows):
            groups = TimingGUI.sensor_groups(Mock())
        self.assertEqual([name for name, _ in groups], ["Voltages"])
