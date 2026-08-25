import unittest

import ddr5_telemetry as t


class AdcDecodeTest(unittest.TestCase):
    """JESD301-2 Table 137: 15 mV per LSB, except the bulk input at 70 mV."""

    def test_rail_and_internal_channels_step_15_mv(self):
        self.assertEqual(t.decode_adc_mv(t.ADC_SWA_VDD, 101), 1515)
        self.assertEqual(t.decode_adc_mv(t.ADC_SWB_VDDQ, 96), 1440)
        self.assertEqual(t.decode_adc_mv(t.ADC_SWC_VPP, 120), 1800)
        self.assertEqual(t.decode_adc_mv(t.ADC_VOUT_1V8, 120), 1800)
        self.assertEqual(t.decode_adc_mv(t.ADC_VOUT_1V0, 67), 1005)

    def test_the_bulk_input_steps_70_mv(self):
        self.assertEqual(t.decode_adc_mv(t.ADC_VIN_BULK, 70), 4900)

    def test_a_zero_sample_is_an_idle_channel_not_zero_volts(self):
        self.assertIsNone(t.decode_adc_mv(t.ADC_SWA_VDD, 0))

    def test_an_unknown_channel_decodes_to_nothing(self):
        self.assertIsNone(t.decode_adc_mv(0xF, 100))

    def test_an_implausible_sample_is_rejected(self):
        # 70 mV per LSB tops out well past any DIMM rail.
        self.assertIsNone(t.decode_adc_mv(t.ADC_VIN_BULK, 255))

    def test_the_select_byte_carries_the_enable_bit_and_the_code(self):
        self.assertEqual(t.select_byte(t.ADC_VIN_BULK), 0x80 | (0x5 << 3))
        self.assertEqual(t.select_byte(t.ADC_VOUT_1V0), 0x80 | (0x9 << 3))


class CurrentLimitTest(unittest.TestCase):
    def test_the_bench_register_decodes_to_its_three_limits(self):
        # R20h = 0xCF on the bench: SWA and SWB at 6 A, SWC at 1.25 A.
        self.assertEqual(t.decode_current_limits(0xCF), (6000, 6000, 1250))

    def test_each_field_reads_its_own_bits(self):
        self.assertEqual(t.decode_current_limits(0x00), (3000, 3000, 500))
        self.assertEqual(t.decode_current_limits(0x40), (4000, 3000, 500))
        self.assertEqual(t.decode_current_limits(0x04), (3000, 4000, 500))
        self.assertEqual(t.decode_current_limits(0x01), (3000, 3000, 750))


class PowerDecodeTest(unittest.TestCase):
    """Two modes, and the registers do not say which; R1Bh bit 6 does."""

    BENCH = {
        t.SWA_TELEMETRY_REGISTER: 3,
        t.SWB_TELEMETRY_REGISTER: 1,
        t.SWC_TELEMETRY_REGISTER: 0,
        t.TELEMETRY_MODE_REGISTER: 0x05,      # bit 6 clear -> current mode
        t.TELEMETRY_TOTAL_REGISTER: 0x00,
        t.CURRENT_LIMIT_REGISTER: 0xCF,
        t.RAIL_CONFIG_REGISTER: 0x80,         # bit 3 clear -> one phase
    }
    VOLTS = {"vdd": 1.5, "vddq": 1.44, "vpp": 1.8}

    def test_current_mode_reproduces_the_bench_reading(self):
        # ZenTimings showed 0.242 W as the minimum over the same window.
        swa, swb, swc, total = t.decode_power_watts(self.BENCH, self.VOLTS)
        self.assertAlmostEqual(swa, 3 * 6.0 / 256 * 1.5, places=6)
        self.assertAlmostEqual(swb, 1 * 6.0 / 64 * 1.44, places=6)
        self.assertEqual(swc, 0.0)
        self.assertAlmostEqual(total, 0.2405, places=3)

    def test_dual_phase_doubles_the_swa_rail(self):
        registers = {**self.BENCH, t.RAIL_CONFIG_REGISTER: 0x88}
        single = t.decode_power_watts(self.BENCH, self.VOLTS)[0]
        dual = t.decode_power_watts(registers, self.VOLTS)[0]
        self.assertAlmostEqual(dual, single * 2, places=6)

    def test_power_mode_reads_the_registers_as_watts(self):
        registers = {**self.BENCH, t.TELEMETRY_MODE_REGISTER: 0x45}
        swa, swb, swc, total = t.decode_power_watts(registers, self.VOLTS)
        self.assertAlmostEqual(swa, 0.375)
        self.assertAlmostEqual(swb, 0.125)
        self.assertAlmostEqual(swc, 0.0)
        self.assertAlmostEqual(total, 0.5)

    def test_a_combined_total_is_not_counted_twice(self):
        # R1Ah bit 1: SWA carries the total, so its own share is the remainder.
        registers = {
            **self.BENCH,
            t.TELEMETRY_MODE_REGISTER: 0x45,
            t.TELEMETRY_TOTAL_REGISTER: 0x02,
            t.SWA_TELEMETRY_REGISTER: 8,
        }
        swa, swb, swc, total = t.decode_power_watts(registers, self.VOLTS)
        self.assertAlmostEqual(total, 1.0)
        self.assertAlmostEqual(swa, 1.0 - swb - swc)

    def test_missing_registers_decode_to_nothing(self):
        self.assertIsNone(t.decode_power_watts({}, self.VOLTS))
        partial = {k: v for k, v in self.BENCH.items()
                   if k != t.CURRENT_LIMIT_REGISTER}
        self.assertIsNone(t.decode_power_watts(partial, self.VOLTS))


class VendorTest(unittest.TestCase):
    def test_the_bench_pmic_identifies_itself(self):
        # R3Ch/R3Dh 0x8A/0x8C, R3Bh 0x12 -> Richtek Power rev 2.1, which is
        # what ZenTimings shows for the same part.
        self.assertEqual(t.decode_pmic_vendor(0x8A, 0x8C), "Richtek Power")
        self.assertEqual(t.decode_pmic_revision(0x12), "2.1")

    def test_an_unlisted_vendor_reports_its_raw_id(self):
        self.assertEqual(t.decode_pmic_vendor(0x80, 0x11), "0x8011")

    def test_nothing_read_reports_nothing(self):
        self.assertIsNone(t.decode_pmic_vendor(None, 0x8C))
        self.assertIsNone(t.decode_pmic_revision(None))


class FakeReader:
    """An SMBus stand-in that records writes and answers from a register map."""

    def __init__(self, registers=None, adc=None, driver_open=True):
        self.registers = dict(registers or {})
        self.adc = dict(adc or {})
        self.writes = []
        self.selected = None
        self._driver_open = driver_open

    def is_driver_open(self):
        return self._driver_open

    def write_byte(self, address, register, value, controller=0x00):
        if register != t.TELEMETRY_SELECT_REGISTER:
            raise ValueError("only the telemetry selector may be written")
        self.writes.append((address, register, value))
        self.selected = (value >> 3) & 0x0F

    def read_byte(self, address, register, controller=0x00):
        if register == t.TELEMETRY_VALUE_REGISTER:
            return self.adc.get(self.selected, 0)
        if register in self.registers:
            return self.registers[register]
        raise OSError("no such register")


class ReadAdcTest(unittest.TestCase):
    def test_the_channel_is_selected_then_read_back(self):
        reader = FakeReader(adc={t.ADC_VIN_BULK: 70})
        slept = []
        millivolts = t.read_adc_millivolts(
            reader, 0x49, t.ADC_VIN_BULK, sleep=slept.append
        )
        self.assertEqual(millivolts, 4900)
        self.assertEqual(reader.writes,
                         [(0x49, 0x30, t.select_byte(t.ADC_VIN_BULK))])
        self.assertEqual(slept, [t.TELEMETRY_SETTLE_SECONDS])

    def test_a_stale_sample_is_never_the_one_returned(self):
        # The bug this guards: at the 9 ms the spec asks for, R31h still held
        # the previous channel's conversion often enough to rotate the
        # readings -- VDDQ showing VDD's value. A stale sample is a real
        # voltage from the wrong rail, so nothing downstream can spot it.
        class Lagging(FakeReader):
            def __init__(self):
                super().__init__()
                self.samples = [100, 96]      # stale, then the real one

            def read_byte(self, address, register, controller=0x00):
                if register == t.TELEMETRY_VALUE_REGISTER:
                    return self.samples.pop(0) if self.samples else 96
                return super().read_byte(address, register, controller)

        reader = Lagging()
        millivolts = t.read_adc_millivolts(
            reader, 0x49, t.ADC_SWB_VDDQ, sleep=lambda _s: None
        )
        self.assertEqual(millivolts, 96 * 15)

    def test_the_settle_clears_the_spec_floor(self):
        self.assertGreaterEqual(t.TELEMETRY_SETTLE_SECONDS, 0.009)

    def test_a_refused_write_reports_no_reading(self):
        class Refusing(FakeReader):
            def write_byte(self, *args, **kwargs):
                raise ValueError("not permitted")

        self.assertIsNone(t.read_adc_millivolts(
            Refusing(), 0x49, t.ADC_VIN_BULK, sleep=lambda _s: None
        ))

    def test_only_the_selector_is_ever_written(self):
        reader = FakeReader(adc={t.ADC_SWA_VDD: 100},
                            registers=dict(PowerDecodeTest.BENCH))
        t.read_pmic_telemetry(reader, 0x49, sleep=lambda _s: None)
        self.assertTrue(reader.writes)
        for _address, register, _value in reader.writes:
            self.assertEqual(register, t.TELEMETRY_SELECT_REGISTER)


class BackendCacheTest(unittest.TestCase):
    """Resolving the transport must not cost a WMI round trip per poll."""

    def setUp(self):
        self._saved = list(t._BACKEND)
        t._BACKEND.clear()
        self.addCleanup(
            lambda: (t._BACKEND.clear(), t._BACKEND.extend(self._saved))
        )

    def test_the_platform_is_asked_once(self):
        # detect_current_platform opens a WMI connection and runs three
        # queries -- about a second on the bench, against a one-second poll.
        calls = []
        import platform_profiles

        original = platform_profiles.detect_current_platform

        def counting(*args, **kwargs):
            calls.append(1)
            return platform_profiles.UNSUPPORTED

        platform_profiles.detect_current_platform = counting
        self.addCleanup(
            setattr, platform_profiles, "detect_current_platform", original
        )
        import sys

        saved = sys.modules.pop("timings", None)
        if saved is not None:
            self.addCleanup(sys.modules.__setitem__, "timings", saved)

        for _ in range(5):
            t.default_smbus_backend()
        self.assertEqual(len(calls), 1)

    def test_an_unsupported_machine_is_cached_too(self):
        # Otherwise the one case with nothing to show pays the most for it.
        import platform_profiles
        import sys

        original = platform_profiles.detect_current_platform
        platform_profiles.detect_current_platform = (
            lambda *a, **k: platform_profiles.UNSUPPORTED
        )
        self.addCleanup(
            setattr, platform_profiles, "detect_current_platform", original
        )
        saved = sys.modules.pop("timings", None)
        if saved is not None:
            self.addCleanup(sys.modules.__setitem__, "timings", saved)

        self.assertIsNone(t.default_smbus_backend())
        self.assertEqual(len(t._BACKEND), 1)
        self.assertIsNone(t.default_smbus_backend())


class IntelBusTest(unittest.TestCase):
    """The DDR5 devices decode the same on either host controller."""

    HUBS = tuple(range(0x50, 0x58))
    PMICS = tuple(range(0x48, 0x50))

    def test_the_pairing_holds_on_the_intel_address_lists(self):
        # The Z790 bench answers at the first slot of each channel, where the
        # AM5 bench answers at the second. Same slot-position rule either way.
        self.assertEqual(
            t.pmic_address_for_hub(0x50, self.HUBS, self.PMICS), 0x48
        )
        self.assertEqual(
            t.pmic_address_for_hub(0x52, self.HUBS, self.PMICS), 0x4A
        )

    def test_a_hub_past_the_list_pairs_with_nothing(self):
        self.assertIsNone(
            t.pmic_address_for_hub(0x60, self.HUBS, self.PMICS)
        )

    def test_one_controller_is_walked_when_the_platform_has_one(self):
        # The PCH exposes a single controller. Passing (0x00,) must not send
        # the scan looking for a second one that does not exist.
        seen = []

        class Probing(FakeReader):
            def probe_address(self, address, controller=0x00, register=0x00):
                seen.append((controller, address))
                return False

        t.read_dimm_telemetry(
            reader_factory=lambda: Probing(),
            controllers=(0x00,),
            addresses=self.HUBS,
            pmic_addresses=self.PMICS,
        )
        self.assertTrue(seen)
        self.assertEqual({controller for controller, _ in seen}, {0x00})

    def test_a_populated_slot_reports_its_own_pmic(self):
        registers = {
            t.SWA_TELEMETRY_REGISTER: 3, t.SWB_TELEMETRY_REGISTER: 4,
            t.SWC_TELEMETRY_REGISTER: 0, t.TELEMETRY_MODE_REGISTER: 0x05,
            t.TELEMETRY_TOTAL_REGISTER: 0x00, t.CURRENT_LIMIT_REGISTER: 0xCF,
            t.RAIL_CONFIG_REGISTER: 0xB0,
        }

        class OneSlot(FakeReader):
            def probe_address(self, address, controller=0x00, register=0x00):
                return address in (0x50, 0x48)

        entries = t.read_dimm_telemetry(
            reader_factory=lambda: OneSlot(registers=registers),
            sleep=lambda _s: None,
            controllers=(0x00,),
            addresses=self.HUBS,
            pmic_addresses=self.PMICS,
        )
        self.assertEqual(len(entries), 1)
        self.assertEqual(entries[0]["channel"], "a")
        self.assertEqual(entries[0]["hub_address"], 0x50)
        self.assertEqual(entries[0]["pmic_address"], 0x48)


class PmicPairingTest(unittest.TestCase):
    def test_a_hub_pairs_with_the_pmic_in_the_same_slot(self):
        # The bench: hubs 0x51 and 0x53 with PMICs 0x49 and 0x4B.
        self.assertEqual(t.pmic_address_for_hub(0x51), 0x49)
        self.assertEqual(t.pmic_address_for_hub(0x53), 0x4B)
        self.assertEqual(t.pmic_address_for_hub(0x50), 0x48)

    def test_an_address_outside_the_hub_range_pairs_with_nothing(self):
        self.assertIsNone(t.pmic_address_for_hub(0x60))


if __name__ == "__main__":
    unittest.main()
