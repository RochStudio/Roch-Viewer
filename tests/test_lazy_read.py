import importlib
import sys
import unittest


class LazyReadImportTest(unittest.TestCase):
    def test_import_does_not_load_hardware_driver_module(self):
        sys.modules.pop("read", None)
        sys.modules.pop("lazy_read", None)
        importlib.import_module("lazy_read")
        self.assertNotIn("read", sys.modules)


if __name__ == "__main__":
    unittest.main()
