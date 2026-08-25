import importlib
import sys
import unittest
from unittest import mock

from platform_profiles import UNSUPPORTED


class BackendDispatchTest(unittest.TestCase):
    def test_an_unrecognised_platform_does_not_import_the_intel_backend(self):
        # The dispatcher is the safety boundary: MCHBAR readers must not run
        # on a machine that is not the socket they were written for. It used
        # to be checked by dispatching to the AMD backend, which this tree
        # does not carry, so it is checked against an unsupported machine
        # instead -- the same boundary, from the other side.
        sys.modules.pop("timings", None)
        sys.modules.pop("intel_timings", None)
        with mock.patch(
            "platform_profiles.detect_current_platform", return_value=UNSUPPORTED
        ):
            module = importlib.import_module("timings")
        self.assertEqual(module.ACTIVE_PLATFORM, UNSUPPORTED)
        self.assertNotIn("intel_timings", sys.modules)
        # And the tab is not left empty: it says why rather than showing rows.
        status = next(row for row in module.TIMINGS
                      if row["name"] == "Read Status")
        self.assertIn("unsupported", status["value"].lower())

    def tearDown(self):
        # Re-import for this machine, so the modules the rest of the suite
        # shares are the real ones rather than whatever this test dispatched.
        sys.modules.pop("timings", None)
        importlib.import_module("timings")

    def test_unsupported_backend_is_neutral(self):
        import timings

        rows, _formula = timings.load_timing_backend(UNSUPPORTED)
        status = next(row for row in rows if row["name"] == "Read Status")
        self.assertIn("unsupported", status["value"].lower())


if __name__ == "__main__":
    unittest.main()
