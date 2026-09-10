import unittest
from unittest.mock import Mock
from rochviewer.ui.main import TimingGUI


class ModuleSelectorTest(unittest.TestCase):
    def test_all_modules_label_names_both_installed_sticks(self):
        self.assertEqual(
            TimingGUI._all_modules_label(
                [("A1", "First stick")], [("B1", "Second stick")]
            ),
            "A1: First stick  |  B1: Second stick",
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
        label.configure.assert_called_with(text="A 34  |  B 36")
        gui.module_selector.set.assert_called_with("A1: stick  |  B1: stick")
        gui._select_memory_module("unknown")
        self.assertIsNone(gui.selected_module_channel)

    def test_missing_channel_does_not_use_other_channel(self):
        gui = TimingGUI.__new__(TimingGUI)
        gui.selected_module_channel = "b"
        self.assertEqual(gui._read_compact_value(
            {"value_a": lambda: 34, "value_b": lambda: "—"}), "—")
