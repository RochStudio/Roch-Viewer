import struct
import unittest

from rochviewer.amd import smn_mcfg as amd_smn_mcfg


class FakeConfigIO:
    def __init__(self, vendor=0x1022, smn=None, open_=True):
        self.vendor = vendor
        self.smn = dict(smn or {})
        self.open_ = open_
        self.selector = 0xABCDEF00
        self.calls = []

    def is_driver_open(self):
        self.calls.append(("is_open",))
        return self.open_

    def read_dword(self, register):
        self.calls.append(("read", register))
        if register == 0:
            return 0x12340000 | self.vendor
        if register == 0x60:
            return self.selector
        if register == 0x64:
            return self.smn.get(self.selector, 0)
        raise AssertionError(register)

    def write_selector(self, value):
        self.calls.append(("write_selector", value))
        self.selector = value


class FakeCtypesFunction:
    def __init__(self, result=True):
        self.result = result
        self.calls = []
        self.argtypes = None
        self.restype = None

    def __call__(self, *args):
        self.calls.append(args)
        return self.result


class FakeDll:
    def __init__(self):
        self.GetPhysLong = FakeCtypesFunction()
        self.SetPhysLong = FakeCtypesFunction()


class FakePortIOWithDll:
    def __init__(self):
        self._dll = FakeDll()

    def is_driver_open(self):
        return True


def _mcfg_table(base=0xE0000000):
    header = b"MCFG" + struct.pack("<I", 60) + bytes(28)
    return header + bytes(8) + struct.pack("<QHBBI", base, 0, 0, 0xFF, 0)


class FakeMutex:
    def __init__(self):
        self.entered = self.exited = 0

    def __enter__(self):
        self.entered += 1
        return self

    def __exit__(self, *_):
        self.exited += 1


class McfgSmnReaderTest(unittest.TestCase):
    def test_reads_batch_and_restores_prior_selector(self):
        config = FakeConfigIO(smn={0x50204: 0x1111, 0x50208: 0x2222})
        mutex = FakeMutex()
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=mutex)
        self.assertEqual(
            reader.read_many((0x50204, 0x50208)),
            {0x50204: 0x1111, 0x50208: 0x2222},
        )
        self.assertEqual(config.selector, 0xABCDEF00)
        self.assertEqual((mutex.entered, mutex.exited), (1, 1))
        writes = [call for call in config.calls if call[0] == "write_selector"]
        self.assertEqual(writes, [
            ("write_selector", 0x50204),
            ("write_selector", 0x50208),
            ("write_selector", 0xABCDEF00),
        ])

    def test_foreign_vendor_prevents_selector_write(self):
        config = FakeConfigIO(vendor=0x8086)
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=FakeMutex())
        self.assertIsNone(reader.read(0x50204))
        self.assertIn("vendor", reader.last_error.lower())
        self.assertFalse(any(call[0] == "write_selector" for call in config.calls))

    def test_closed_driver_prevents_config_access(self):
        config = FakeConfigIO(open_=False)
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=FakeMutex())
        self.assertIsNone(reader.read(0x50204))
        self.assertEqual(config.calls, [("is_open",)])

    def test_has_no_public_smn_write_api(self):
        self.assertFalse(hasattr(amd_smn_mcfg.McfgSmnReader, "write"))
        self.assertFalse(hasattr(amd_smn_mcfg.McfgSmnReader, "smn_write"))

    def test_all_ones_invalidates_the_complete_batch(self):
        config = FakeConfigIO(smn={0x50204: 0x1111, 0x50208: 0xFFFFFFFF})
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=FakeMutex())
        self.assertEqual(
            reader.read_many((0x50204, 0x50208)),
            {0x50204: None, 0x50208: None},
        )
        self.assertIn("0xffffffff", reader.last_error.lower())
        self.assertEqual(config.selector, 0xABCDEF00)

    def test_failed_selector_write_still_attempts_restore(self):
        class FailingConfig(FakeConfigIO):
            def __init__(self):
                super().__init__()
                self.failed = False

            def write_selector(self, value):
                super().write_selector(value)
                if not self.failed and value == 0x50204:
                    self.failed = True
                    raise OSError("reported write failure")

        config = FailingConfig()
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=FakeMutex())
        self.assertIsNone(reader.read(0x50204))
        self.assertEqual(config.selector, 0xABCDEF00)
        self.assertEqual(
            [call for call in config.calls if call[0] == "write_selector"],
            [
                ("write_selector", 0x50204),
                ("write_selector", 0xABCDEF00),
            ],
        )

    def test_real_config_transport_exposes_selector_write_only(self):
        self.assertTrue(hasattr(amd_smn_mcfg.InpOutMcfgConfigIO, "write_selector"))
        self.assertFalse(hasattr(amd_smn_mcfg.InpOutMcfgConfigIO, "write_dword"))
        self.assertFalse(
            hasattr(amd_smn_mcfg.InpOutPhysicalAccess, "_write_dword")
        )

    def test_real_config_transport_hardcodes_ecam_selector_destination(self):
        port_io = FakePortIOWithDll()
        config = amd_smn_mcfg.InpOutMcfgConfigIO(
            port_io=port_io, table=_mcfg_table()
        )
        config.write_selector(0x50204)
        self.assertEqual(len(port_io._dll.SetPhysLong.calls), 1)
        address, value = port_io._dll.SetPhysLong.calls[0]
        self.assertEqual(address.value, 0xE0000060)
        self.assertEqual(value.value, 0x50204)

    def test_config_dword_reads_require_aligned_registers(self):
        config = object.__new__(amd_smn_mcfg.InpOutMcfgConfigIO)
        config.config_base = 0xE0000000
        config._physical = object()
        for register in (-4, 1, 0x1000):
            with self.subTest(register=register), self.assertRaises(ValueError):
                config.read_dword(register)

    def test_restore_failure_rejects_complete_snapshot(self):
        class RestoreFailConfig(FakeConfigIO):
            def write_selector(self, value):
                if value == 0xABCDEF00 and self.selector != value:
                    raise OSError("restore failed")
                super().write_selector(value)

        config = RestoreFailConfig(smn={0x50204: 0x1111})
        reader = amd_smn_mcfg.McfgSmnReader(config, mutex=FakeMutex())
        self.assertIsNone(reader.read(0x50204))
        self.assertIn("restore failed", reader.last_error.lower())


if __name__ == "__main__":
    unittest.main()
