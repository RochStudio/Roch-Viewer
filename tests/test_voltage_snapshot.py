import unittest
from unittest.mock import Mock, patch
from rochviewer.amd.profile import _voltage_snapshot_rows, _dram_ratio
from rochviewer.ui.display_values import select_tab_names


class VoltageSnapshotTest(unittest.TestCase):
    def test_snapshot_is_once_and_not_live(self):
        with patch('rochviewer.amd.profile._format_voltage', return_value=lambda: '1.250 V'), \
             patch('rochviewer.amd.profile._import_call', return_value=[
                 {'channel': 'a', 'vdd': 1425}, {'channel': 'b', 'vdd': 1410}
             ]) as native:
            rows = _voltage_snapshot_rows(Mock())
            native.assert_not_called()
            by_name = {r['name']: r for r in rows}
            self.assertEqual(by_name['VDDCR_VDD snapshot']['display_name'], 'VDDCR_VDD')
            self.assertEqual(by_name['VTT snapshot']['display_name'], 'VTT')
            self.assertFalse(any(n.endswith(' voltage') for n in by_name))
            self.assertEqual(by_name['CHA VDD']['value'](), '1.425 V')
            self.assertEqual(by_name['CHB VDD']['value'](), '1.410 V')
            self.assertEqual(by_name['CHA VDDQ']['value'](), '—')
            by_name['CHA VDD']['value']()
            native.assert_called_once()
            self.assertTrue(all(not r.get('live') for r in rows))
            self.assertIn('Voltages', select_tab_names(rows))

    def test_voltage_tab_uses_continuous_row_shading(self):
        from rochviewer.ui.main import SHADED_TABS, CONTINUOUS_SECTION_TABS
        self.assertIn('Voltages', SHADED_TABS)
        self.assertIn('Voltages', CONTINUOUS_SECTION_TABS)

    def test_summary_has_four_columns_with_snapshot_rails(self):
        from rochviewer.ui.main import summary_column_count
        self.assertEqual(summary_column_count(_voltage_snapshot_rows(Mock())), 4)
        self.assertEqual(summary_column_count([]), 3)

    def test_summary_keeps_diagnostic_voltage_rows_on_the_voltage_tab_only(self):
        from rochviewer.ui.main import summary_snapshot_voltage_rows
        rows = _voltage_snapshot_rows(Mock())
        summary_names = {row['name'] for row in summary_snapshot_voltage_rows(rows)}
        self.assertNotIn('VTT snapshot', summary_names)
        for channel in ('CHA', 'CHB'):
            self.assertNotIn(channel + ' VIN', summary_names)
            self.assertNotIn(channel + ' 1.8V output', summary_names)
            self.assertNotIn(channel + ' 1.0V output', summary_names)
            self.assertIn(channel + ' VDD', summary_names)
        all_voltage_names = {row['name'] for row in rows}
        self.assertIn('VTT snapshot', all_voltage_names)
        self.assertIn('CHA VIN', all_voltage_names)

    def test_effective_ddr_ratio(self):
        runtime = Mock()
        runtime.value.return_value = 3000
        with patch('rochviewer.amd.profile._processor_facts', return_value={'ext_clock': 100}):
            self.assertEqual(_dram_ratio(runtime), '60.00')
