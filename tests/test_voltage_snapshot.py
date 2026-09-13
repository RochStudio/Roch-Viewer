import unittest
from unittest.mock import Mock, patch
from rochviewer.amd.profile import _voltage_snapshot_rows, _dram_ratio
from rochviewer.ui.display_values import select_tab_names
from tests.intel_stub import install, restore


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

    def test_summary_stays_three_columns_with_snapshot_rails(self):
        from rochviewer.ui.main import summary_column_count
        self.assertEqual(summary_column_count(_voltage_snapshot_rows(Mock())), 3)
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


class IntelVoltageSnapshotTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.intel = install()

    @classmethod
    def tearDownClass(cls):
        restore()

    def test_snapshot_reuses_voltage_readers_once_and_is_not_live(self):
        first = Mock(return_value='1.250 V')
        second = Mock(return_value='1.350 V')
        rows = (
            ('Vcore', 'Voltages', first, 'Left'),
            ('VDD2', 'Voltages', second, 'Right'),
            ('CPU Temp', 'Thermal & Power', Mock(), 'Right'),
        )
        with patch.object(self.intel, 'SENSOR_ROWS', rows):
            snapshots = self.intel._voltage_snapshot_rows('test-platform')

        by_name = {row['name']: row for row in snapshots}
        self.assertEqual(by_name['Vcore snapshot']['display_name'], 'Vcore')
        self.assertEqual(by_name['Vcore snapshot']['value'](), '1.250 V')
        self.assertEqual(by_name['VDD2 snapshot']['value'](), '1.350 V')
        by_name['Vcore snapshot']['value']()
        first.assert_called_once_with()
        second.assert_called_once_with()
        self.assertTrue(all(not row.get('live') for row in snapshots))
        self.assertNotIn('CPU Temp snapshot', by_name)
        self.assertIn('Voltages', select_tab_names(snapshots))

    def test_snapshot_obeys_platform_absent_rows_and_labels(self):
        rows = (
            ('DLVR Vcore', 'Voltages', Mock(return_value='1.2 V'), 'Left'),
            ('VCCIO', 'Voltages', Mock(return_value='1.0 V'), 'Left'),
        )
        with patch.object(self.intel, 'SENSOR_ROWS', rows):
            snapshots = self.intel._voltage_snapshot_rows(
                'lga1700-ddr5', absent=('VCCIO',)
            )
        names = {row['name'] for row in snapshots}
        self.assertIn('Vcore snapshot', names)
        self.assertNotIn('VCCIO snapshot', names)

    def test_intel_snapshot_keeps_the_fixed_summary_width(self):
        from rochviewer.ui.main import summary_column_count

        rows = (
            ('Vcore', 'Voltages', Mock(return_value='1.2 V'), 'Left'),
        )
        with patch.object(self.intel, 'SENSOR_ROWS', rows):
            snapshots = self.intel._voltage_snapshot_rows('test-platform')
        self.assertEqual(summary_column_count(snapshots), 3)

    def test_intel_snapshot_includes_each_dimms_measured_rails(self):
        modules = Mock(return_value=[
            {
                'channel': 'a', 'vdd': 1440, 'vddq': 1410, 'vpp': 1800,
                'vin_bulk': 5040, 'vout_1v8': 1800, 'vout_1v0': 990,
            },
            {
                'channel': 'b', 'vdd': 1425, 'vddq': 1395, 'vpp': 1815,
                'vin_bulk': 5110, 'vout_1v8': 1815, 'vout_1v0': 1005,
            },
        ])
        with patch.object(self.intel, 'SENSOR_ROWS', ()):
            rows = self.intel._voltage_snapshot_rows(
                self.intel.LGA1700_DDR5, read_dimms=modules
            )

        by_name = {row['name']: row for row in rows}
        self.assertEqual(by_name['CHA VDD']['value'](), '1.440 V')
        self.assertEqual(by_name['CHA VDDQ']['value'](), '1.410 V')
        self.assertEqual(by_name['CHA VPP']['value'](), '1.800 V')
        self.assertEqual(by_name['CHB VIN']['value'](), '5.110 V')
        self.assertEqual(by_name['CHB 1.8V output']['value'](), '1.815 V')
        self.assertEqual(by_name['CHB 1.0V output']['value'](), '1.005 V')
        by_name['CHB VDD']['value']()
        modules.assert_called_once_with()
        self.assertTrue(all(not row.get('live') for row in rows))

    def test_multiple_dimms_on_one_channel_are_not_collapsed(self):
        modules = Mock(return_value=[
            {'channel': 'a', 'vdd': 1440},
            {'channel': 'a', 'vdd': 1425},
        ])
        with patch.object(self.intel, 'SENSOR_ROWS', ()):
            rows = self.intel._voltage_snapshot_rows(
                self.intel.LGA1700_DDR5, read_dimms=modules
            )
        by_name = {row['name']: row for row in rows}
        self.assertEqual(by_name['CHA VDD']['value'](), '\u2014')

    def test_ddr4_does_not_show_nonexistent_pmic_rows(self):
        modules = Mock()
        with patch.object(self.intel, 'SENSOR_ROWS', ()):
            rows = self.intel._voltage_snapshot_rows(
                self.intel.LGA1700_DDR4, read_dimms=modules
            )
        self.assertEqual([row['name'] for row in rows], ['Reading mode'])
        modules.assert_not_called()
