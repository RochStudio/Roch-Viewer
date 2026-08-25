"""Transport-level tests for the AM5 SMN reader.

These never touch hardware: a FakePortIO emulates the AMD PCI CF8/CFC
config mechanism (bus/dev/func 00:00.0) and records every port access so
the exact transport sequence, CF8 save/restore, vendor gating and failure
handling can be asserted.
"""

import unittest

from rochviewer.amd import smn as amd_smn


PCI_ENABLE = 0x80000000
CF8 = 0xCF8
CFC = 0xCFC
VENDOR_REG = 0x00
SELECTOR_REG = 0x60
DATA_REG = 0x64
AMD_VENDOR = 0x1022


def _cfg(reg):
    return PCI_ENABLE | (reg & 0xFC)


class FakePortIO:
    """Emulates 00:00.0 config space + the SMN index/data window."""

    def __init__(self, smn=None, vendor=AMD_VENDOR, driver_open=True,
                 initial_cf8=0xDEADBEEF, initial_selector=0xABCDEF00):
        self.smn = dict(smn or {})
        self.vendor = vendor
        self.driver_open = driver_open
        self.cf8 = initial_cf8 & 0xFFFFFFFF
        self.selector = initial_selector & 0xFFFFFFFF
        self.calls = []

    # -- injected interface -------------------------------------------------
    def is_driver_open(self):
        self.calls.append(("is_open",))
        return self.driver_open

    def outl(self, port, value):
        value &= 0xFFFFFFFF
        self.calls.append(("outl", port, value))
        if port == CF8:
            self.cf8 = value
        elif port == CFC:
            if self.cf8 == _cfg(SELECTOR_REG):
                self.selector = value
            # a write while pointed at DATA_REG would be an illegal SMN write
        else:
            raise AssertionError("unexpected outl port 0x%X" % port)

    def inl(self, port):
        self.calls.append(("inl", port))
        if port == CF8:
            return self.cf8
        if port == CFC:
            if self.cf8 == _cfg(VENDOR_REG):
                return self.vendor & 0xFFFF
            if self.cf8 == _cfg(SELECTOR_REG):
                return self.selector
            if self.cf8 == _cfg(DATA_REG):
                return self.smn.get(self.selector, 0) & 0xFFFFFFFF
            return 0
        raise AssertionError("unexpected inl port 0x%X" % port)


class FakeMutex:
    def __init__(self):
        self.entered = 0
        self.exited = 0

    def __enter__(self):
        self.entered += 1
        return self

    def __exit__(self, *exc):
        self.exited += 1
        return False


class FakeKernel32:
    def __init__(self, wait_result):
        self.wait_result = wait_result
        self.released = 0
        self.closed = 0

    def CreateMutexW(self, _security, _owned, _name):
        return 123

    def WaitForSingleObject(self, _handle, _timeout):
        return self.wait_result

    def ReleaseMutex(self, _handle):
        self.released += 1
        return True

    def CloseHandle(self, _handle):
        self.closed += 1
        return True


class NamedMutexTest(unittest.TestCase):
    def _mutex(self, wait_result):
        mutex = object.__new__(amd_smn.NamedMutex)
        mutex._name = "test"
        mutex._timeout_ms = 1
        mutex._handle = None
        mutex._owned = False
        mutex._k32 = FakeKernel32(wait_result)
        return mutex

    def test_timeout_raises_without_releasing_unowned_mutex(self):
        mutex = self._mutex(amd_smn.WAIT_TIMEOUT)
        with self.assertRaises(TimeoutError):
            mutex.__enter__()
        self.assertEqual(mutex._k32.released, 0)
        self.assertEqual(mutex._k32.closed, 1)

    def test_abandoned_mutex_is_owned_and_released(self):
        mutex = self._mutex(amd_smn.WAIT_ABANDONED)
        with mutex:
            pass
        self.assertEqual(mutex._k32.released, 1)
        self.assertEqual(mutex._k32.closed, 1)


class InpOutPortIOTest(unittest.TestCase):
    def test_missing_dll_raises_catchable_exception(self):
        with self.assertRaises(FileNotFoundError):
            amd_smn.InpOutPortIO(dll_path="definitely-missing-inpoutx64.dll")


class SmnReadTest(unittest.TestCase):
    def test_all_ones_invalidates_complete_batch(self):
        io = FakePortIO(smn={0x50204: 0x1111, 0x50208: 0xFFFFFFFF})
        reader = amd_smn.SmnReader(io=io, mutex=FakeMutex())
        self.assertEqual(
            reader.read_many((0x50204, 0x50208)),
            {0x50204: None, 0x50208: None},
        )
        self.assertIn("0xffffffff", reader.last_error.lower())

    def _reader(self, io):
        return amd_smn.SmnReader(io=io, mutex=FakeMutex())

    def test_returns_mapped_value(self):
        io = FakePortIO(smn={0x50204: 0x1234ABCD})
        self.assertEqual(self._reader(io).read(0x50204), 0x1234ABCD)

    def test_zero_is_not_failure(self):
        io = FakePortIO(smn={0x50204: 0x00000000})
        self.assertEqual(self._reader(io).read(0x50204), 0)

    def test_transport_sequence(self):
        io = FakePortIO(smn={0x50208: 0xCAFE})
        self.assertEqual(self._reader(io).read(0x50208), 0xCAFE)
        self.assertEqual(io.calls, [
            ("is_open",),
            ("inl", CF8),                       # save prior CF8
            ("outl", CF8, _cfg(VENDOR_REG)),    # vendor check
            ("inl", CFC),
            ("outl", CF8, _cfg(SELECTOR_REG)),  # save SMN selector
            ("inl", CFC),
            ("outl", CF8, _cfg(SELECTOR_REG)),  # selector write
            ("outl", CFC, 0x50208),
            ("outl", CF8, _cfg(DATA_REG)),      # data read
            ("inl", CFC),
            ("outl", CF8, _cfg(SELECTOR_REG)),  # restore selector
            ("outl", CFC, 0xABCDEF00),
            ("outl", CF8, 0xDEADBEEF),          # restore prior CF8
        ])

    def test_restores_prior_smn_selector(self):
        io = FakePortIO(
            smn={0x50208: 1}, initial_selector=0x11223344
        )
        self._reader(io).read(0x50208)
        self.assertEqual(io.selector, 0x11223344)

    def test_failed_selector_write_still_attempts_restore(self):
        class FailingPortIO(FakePortIO):
            def __init__(self):
                super().__init__(initial_selector=0x11223344)
                self.failed = False

            def outl(self, port, value):
                super().outl(port, value)
                if (
                    not self.failed
                    and port == CFC
                    and self.cf8 == _cfg(SELECTOR_REG)
                    and value == 0x50208
                ):
                    self.failed = True
                    raise OSError("reported write failure")

        io = FailingPortIO()
        self.assertIsNone(self._reader(io).read(0x50208))
        self.assertEqual(io.selector, 0x11223344)

    def test_restores_prior_cf8(self):
        io = FakePortIO(smn={0x50208: 1}, initial_cf8=0x11223344)
        self._reader(io).read(0x50208)
        self.assertEqual(io.cf8, 0x11223344)
        self.assertEqual(io.calls[-1], ("outl", CF8, 0x11223344))

    def test_vendor_rejection(self):
        io = FakePortIO(smn={0x50208: 0xCAFE}, vendor=0x8086)
        reader = self._reader(io)
        self.assertIsNone(reader.read(0x50208))
        self.assertIn("vendor", reader.last_error.lower())
        # never selects the SMN index register on a foreign vendor
        self.assertNotIn(("outl", CF8, _cfg(SELECTOR_REG)), io.calls)
        # CF8 still restored
        self.assertEqual(io.calls[-1], ("outl", CF8, 0xDEADBEEF))

    def test_only_selector_writes(self):
        io = FakePortIO(smn={0x50208: 0xCAFE})
        self._reader(io).read(0x50208)
        cfc_writes = [c for c in io.calls if c[0] == "outl" and c[1] == CFC]
        # the only data written to CFC is the selector value; the code must
        # never write to the SMN data register.
        self.assertEqual(cfc_writes, [
            ("outl", CFC, 0x50208),
            ("outl", CFC, 0xABCDEF00),
        ])

    def test_driver_closed_is_failure(self):
        io = FakePortIO(smn={0x50208: 0xCAFE}, driver_open=False)
        reader = self._reader(io)
        self.assertIsNone(reader.read(0x50208))
        self.assertIn("driver", reader.last_error.lower())
        # no port I/O performed when the driver is unavailable
        self.assertEqual(io.calls, [("is_open",)])

    def test_no_public_write_api(self):
        self.assertFalse(hasattr(amd_smn.SmnReader, "write"))
        self.assertFalse(hasattr(amd_smn.SmnReader, "smn_write"))

    def test_mutex_wraps_access(self):
        io = FakePortIO(smn={0x50208: 1})
        mutex = FakeMutex()
        amd_smn.SmnReader(io=io, mutex=mutex).read(0x50208)
        self.assertEqual((mutex.entered, mutex.exited), (1, 1))

    def test_batch_holds_mutex_and_checks_vendor_once(self):
        io = FakePortIO(smn={0x50204: 0x1111, 0x50208: 0x2222})
        mutex = FakeMutex()
        reader = amd_smn.SmnReader(io=io, mutex=mutex)
        values = reader.read_many((0x50204, 0x50208))
        self.assertEqual(values, {0x50204: 0x1111, 0x50208: 0x2222})
        self.assertEqual((mutex.entered, mutex.exited), (1, 1))
        vendor_selects = [
            call for call in io.calls
            if call == ("outl", CF8, _cfg(VENDOR_REG))
        ]
        self.assertEqual(len(vendor_selects), 1)
        self.assertEqual(io.calls[-1], ("outl", CF8, 0xDEADBEEF))


if __name__ == "__main__":
    unittest.main()
