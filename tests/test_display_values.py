import unittest

from display_values import resolve_display_value, select_tab_names


class DisplayValueTest(unittest.TestCase):
    def test_calls_lazy_value(self):
        self.assertEqual(resolve_display_value(lambda: 36), "36")

    def test_static_value_is_preserved(self):
        self.assertEqual(resolve_display_value("AMD SMN READ-ONLY"), "AMD SMN READ-ONLY")

    def test_failure_is_neutral(self):
        self.assertEqual(resolve_display_value(lambda: 1 / 0), "—")

    def test_am5_profile_hides_empty_intel_tabs(self):
        rows = [
            {"Tab": "System Info"},
            {"Tab": "Timings"},
        ]
        self.assertEqual(
            select_tab_names(rows), ["Summary", "System Info", "Timings"]
        )


if __name__ == "__main__":
    unittest.main()
