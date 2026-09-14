import unittest
from unittest.mock import Mock
from rochviewer.intel.intel_timings import TIMINGS as INTEL_TIMINGS
from rochviewer.ui.main import TimingGUI


class ModuleSelectorTest(unittest.TestCase):
    def test_module_description_lists_each_requested_identity_field(self):
        self.assertEqual(
            TimingGUI._module_description({
                "part_number": "TMXFL1680838KWK",
                "capacity": "16GB",
                "rank": "SR",
                "rank_count": 1,
                "ic": "SK hynix A-die",
            }),
            "TMXFL1680838KWK (16GB, 1R, SK hynix, A-die)",
        )

    def test_all_modules_label_names_both_installed_sticks(self):
        self.assertEqual(
            TimingGUI._all_modules_label(
                [("A1", "First stick")], [("B1", "Second stick")]
            ),
            "A1:First stick | B1:Second stick",
        )

    def test_selection_switches_channel_and_all_restores_pairs(self):
        gui = TimingGUI.__new__(TimingGUI)
        gui._module_choices = {"All modules": None, "A1: stick": "a", "B1: stick": "b"}
        gui.selected_module_channel = None
        timing = {"name": "tPHYRDL", "value_a": lambda: 34, "value_b": lambda: 36}
        label = Mock()
        gui._module_value_labels = [(label, lambda: gui._read_compact_value(timing))]
        gui.module_selector = Mock()
        gui._all_modules_display = "A1: stick  |  B1: stick"
        gui._select_memory_module("B1: stick")
        label.configure.assert_called_with(text="36")
        gui._select_memory_module("A1: stick")
        label.configure.assert_called_with(text="34")
        gui._select_memory_module("All modules")
        label.configure.assert_called_with(text="34/36")
        gui.module_selector.set.assert_called_with("A1: stick  |  B1: stick")
        gui._select_memory_module("unknown")
        self.assertIsNone(gui.selected_module_channel)

    def test_missing_channel_does_not_use_other_channel(self):
        gui = TimingGUI.__new__(TimingGUI)
        gui.selected_module_channel = "b"
        self.assertEqual(gui._read_compact_value(
            {"value_a": lambda: 34, "value_b": lambda: "—"}), "—")

    def test_one_selection_syncs_every_tab_selector(self):
        gui = TimingGUI.__new__(TimingGUI)
        gui._module_choices = {
            "All modules": None,
            "A1: first": "a",
            "B1: second": "b",
        }
        gui._all_modules_display = "A1: first  |  B1: second"
        gui._module_value_labels = []
        gui._module_filtered_rows = []
        gui.module_selectors = {
            name: Mock() for name in ("Summary", "Timings", "Training", "Misc")
        }

        gui._select_memory_module("B1: second")

        self.assertEqual(gui.selected_module_channel, "b")
        for selector in gui.module_selectors.values():
            selector.set.assert_called_with("B1: second")

    def test_timings_and_training_share_the_compact_all_modules_view(self):
        gui = TimingGUI.__new__(TimingGUI)
        timing = {"value_a": lambda: 38, "value_b": lambda: 40}

        gui.selected_module_channel = None
        self.assertEqual(gui._detail_channel_text(timing, "a"), "38/40")
        self.assertEqual(gui._detail_channel_text(timing, "b"), "")
        self.assertEqual(
            gui._detail_channel_header("A1", "B1", "a"), "All modules"
        )
        self.assertEqual(gui._detail_channel_header("A1", "B1", "b"), "")

        timing["Tab"] = "Timings"
        self.assertEqual(gui._detail_channel_text(timing, "a"), "38/40")
        self.assertEqual(gui._detail_channel_text(timing, "b"), "")
        self.assertEqual(
            gui._detail_channel_header("A1", "B1", "a", "Timings"),
            "All modules",
        )
        self.assertEqual(
            gui._detail_channel_header("A1", "B1", "b", "Timings"), ""
        )

        matching = {"value_a": lambda: 38, "value_b": lambda: 38}
        self.assertEqual(gui._detail_channel_text(matching, "a"), "38")

        gui.selected_module_channel = "b"
        self.assertEqual(gui._detail_channel_text(timing, "a"), "40")
        self.assertEqual(gui._detail_channel_text(timing, "b"), "")
        self.assertEqual(gui._detail_channel_header("A1", "B1", "a"), "B1")
        self.assertEqual(gui._detail_channel_header("A1", "B1", "b"), "")

    def test_mc_rows_map_to_the_physical_module_choices(self):
        self.assertEqual(
            TimingGUI._misc_latency_channel({"name": "RTL MC0 CHB R3"}), "a"
        )
        self.assertEqual(
            TimingGUI._misc_latency_channel({"name": "RTL MC1 CHA R0"}), "b"
        )
        self.assertEqual(TimingGUI._misc_latency_channel({"name": ""}), "all")

    def test_rtl_keeps_every_controller_position_visible(self):
        # Validate the Intel source table directly. The UI's TIMINGS is the
        # backend selected for the host, which is unsupported on CI runners.
        rows = [item for item in INTEL_TIMINGS if item.get("Tab") == "RTL"]
        names = {item.get("name") for item in rows}
        self.assertIn("RTL MC0 CHA R0", names)
        self.assertIn("RTL MC1 CHA R0", names)
        self.assertIn("RTL MC0 CHB R0", names)
        self.assertIn("RTL MC1 CHB R0", names)
        self.assertEqual(len([row for row in rows if row.get("name")]), 32)
        self.assertEqual(len([row for row in rows if not row.get("name")]), 2)
