import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from rochviewer.amd import fch_smbus as amd_fch_smbus
from rochviewer.amd.fch_smbus import (
    ALLOWED_ADDRESSES,
    CONTROL_START,
    FALLBACK_SMBUS_BASE,
    FchSmbusReader,
    PROTOCOL_BYTE_DATA,
    REG_HOST_ADDRESS,
    REG_HOST_COMMAND,
    REG_HOST_CONTROL,
    REG_HOST_DATA0,
    REG_HOST_STATUS,
    STATUS_INTR,
    read_smbus_base,
)
from rochviewer.memory import ddr5_pmic
from rochviewer.memory.ddr5_pmic import CONFIRMED_PMIC_RAILS


class FakeIO:
    """Minimal SMBus host that records every port access."""

    def __init__(self, data_byte=0xA5, pmio=None):
        self.writes = []
        self.data_byte = data_byte
        self.pmio = pmio or {}
        self._pmio_index = None
        self.driver_open = True

    def is_driver_open(self):
        return self.driver_open

    def inb(self, port):
        if port == amd_fch_smbus.PMIO_DATA_PORT:
            return self.pmio.get(self._pmio_index, 0x00)
        offset = port - FALLBACK_SMBUS_BASE
        if offset == REG_HOST_STATUS:
            # Never busy, transfer already complete.
            return STATUS_INTR
        if offset == REG_HOST_DATA0:
            return self.data_byte
        return 0x00

    def outb(self, port, value):
        if port == amd_fch_smbus.PMIO_INDEX_PORT:
            self._pmio_index = value
            return
        self.writes.append((port, value))


def make_reader(io=None, **kwargs):
    io = io or FakeIO()
    return FchSmbusReader(
        io=io, mutex=_NullMutex(), base=FALLBACK_SMBUS_BASE, **kwargs
    ), io


class _NullMutex:
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


class SmbusBaseTest(unittest.TestCase):
    def test_base_comes_from_the_pmio_window(self):
        io = FakeIO(pmio={amd_fch_smbus.PMIO_SMBA_LOW: 0x00,
                          amd_fch_smbus.PMIO_SMBA_HIGH: 0x0B})
        self.assertEqual(read_smbus_base(io), 0x0B00)

    def test_base_is_masked_to_the_controller_granularity(self):
        io = FakeIO(pmio={amd_fch_smbus.PMIO_SMBA_LOW: 0x1F,
                          amd_fch_smbus.PMIO_SMBA_HIGH: 0x0B})
        self.assertEqual(read_smbus_base(io), 0x0B00)

    def test_empty_window_falls_back_to_the_fch_default(self):
        self.assertEqual(read_smbus_base(FakeIO(pmio={})), FALLBACK_SMBUS_BASE)


class SmbusReadOnlyTest(unittest.TestCase):
    def test_only_allowlisted_addresses_reach_the_bus(self):
        reader, io = make_reader()
        # DRAM controller, EC, and random devices must all be refused.
        for address in (0x00, 0x2F, 0x30, 0x44, 0x58, 0x69, 0x77):
            self.assertNotIn(address, ALLOWED_ADDRESSES)
            with self.assertRaises(ValueError):
                reader.read_byte(address, 0x30)
        self.assertEqual(io.writes, [], "a refused address still drove the bus")

    def test_allowlist_covers_only_ddr5_spd_and_pmic(self):
        self.assertEqual(
            ALLOWED_ADDRESSES,
            frozenset(list(range(0x48, 0x50)) + list(range(0x50, 0x58))),
        )

    def test_read_uses_the_read_direction_and_byte_data_protocol(self):
        reader, io = make_reader()
        value = reader.read_byte(0x4B, 0x2E)
        self.assertEqual(value, 0xA5)
        by_port = dict(io.writes)
        # Address register must carry the read bit set.
        self.assertEqual(by_port[FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS],
                         (0x4B << 1) | 1)
        self.assertEqual(by_port[FALLBACK_SMBUS_BASE + REG_HOST_COMMAND], 0x2E)
        self.assertEqual(by_port[FALLBACK_SMBUS_BASE + REG_HOST_CONTROL],
                         PROTOCOL_BYTE_DATA | CONTROL_START)

    def test_every_transfer_sets_the_read_bit(self):
        reader, io = make_reader()
        for address in sorted(ALLOWED_ADDRESSES):
            reader.read_byte(address, 0x00)
        for port, value in io.writes:
            if port == FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS:
                self.assertEqual(value & 1, 1, "a transfer was addressed write")

    def test_status_is_cleared_before_and_after(self):
        reader, io = make_reader()
        reader.read_byte(0x49, 0x30)
        status_writes = [
            v for p, v in io.writes if p == FALLBACK_SMBUS_BASE + REG_HOST_STATUS
        ]
        self.assertEqual(len(status_writes), 2)

    def test_no_pmic_rail_register_is_writable(self):
        # THE critical invariant. R21h/R25h/R27h are the VID registers that set
        # VDD/VDDQ/VPP and R2Bh selects their encoding mode. The PMIC is no
        # longer wholly unwritable -- R30h selects an ADC channel, which is how
        # the measured rails are read -- so this asserts the line that matters:
        # nothing that can change a rail is reachable.
        for address in amd_fch_smbus.PMIC_ADDRESSES:
            for register in amd_fch_smbus.PMIC_RAIL_CONTROL_REGISTERS:
                self.assertNotIn(
                    (address, register), amd_fch_smbus.WRITE_ALLOWLIST
                )

    def test_the_only_writable_pmic_register_is_the_adc_selector(self):
        for address, register in amd_fch_smbus.WRITE_ALLOWLIST:
            if address in amd_fch_smbus.PMIC_ADDRESSES:
                self.assertEqual(
                    register, amd_fch_smbus.PMIC_TELEMETRY_SELECT_REGISTER
                )

    def test_write_allowlist_is_the_two_selectors_and_nothing_else(self):
        expected = frozenset(
            [(a, amd_fch_smbus.SPD_HUB_PAGE_REGISTER) for a in range(0x50, 0x58)]
            + [(a, amd_fch_smbus.PMIC_TELEMETRY_SELECT_REGISTER)
               for a in range(0x48, 0x50)]
        )
        self.assertEqual(amd_fch_smbus.WRITE_ALLOWLIST, expected)

    def test_write_refuses_pmic_rail_registers(self):
        reader, io = make_reader()
        for address in amd_fch_smbus.PMIC_ADDRESSES:
            for register in (0x00, 0x0B, 0x21, 0x25, 0x27, 0x2B, 0x2E, 0x2F):
                with self.assertRaises(ValueError):
                    reader.write_byte(address, register, 0x00)
        self.assertEqual(io.writes, [], "a refused write still drove the bus")

    def test_locked_write_helper_also_enforces_the_allowlist(self):
        # Internal callers take the mutex themselves; they must not be able to
        # bypass the allowlist by using the locked helper directly.
        reader, _ = make_reader()
        with self.assertRaises(ValueError):
            reader._write_byte_locked(0x49, 0x21, 0xFF, 0x00)

    def test_the_only_pmic_write_that_lands_is_the_adc_selector(self):
        # Every other register on the PMIC is refused before the bus is driven,
        # including the ones either side of the selector.
        reader, io = make_reader()
        for register in (0x00, 0x21, 0x25, 0x27, 0x2B, 0x2F, 0x31, 0x3C):
            with self.assertRaises(ValueError):
                reader.write_byte(0x49, register, 0x00)
        self.assertEqual(io.writes, [])

        reader.write_byte(0x49, amd_fch_smbus.PMIC_TELEMETRY_SELECT_REGISTER, 0x80)
        self.assertTrue(io.writes, "the permitted selector write did not happen")

    def test_write_refuses_non_page_registers_on_the_spd_hub(self):
        reader, io = make_reader()
        for register in (0x00, 0x0A, 0x0C, 0x30, 0x80):
            with self.assertRaises(ValueError):
                reader.write_byte(0x51, register, 0x00)
        self.assertEqual(io.writes, [])

    def test_permitted_write_uses_the_write_direction(self):
        reader, io = make_reader()
        reader.write_byte(0x51, amd_fch_smbus.SPD_HUB_PAGE_REGISTER, 0x04)
        by_port = dict(io.writes)
        self.assertEqual(
            by_port[FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS], (0x51 << 1)
        )
        self.assertEqual(by_port[FALLBACK_SMBUS_BASE + REG_HOST_DATA0], 0x04)

    def test_read_spd_restores_the_original_page(self):
        class PagingIO(FakeIO):
            def __init__(self):
                FakeIO.__init__(self)
                self.page_writes = []

            def outb(self, port, value):
                FakeIO.outb(self, port, value)
                if port == FALLBACK_SMBUS_BASE + REG_HOST_DATA0:
                    self.page_writes.append(value)

        io = PagingIO()
        io.data_byte = 0x03          # hub reports it is currently on page 3
        reader, _ = make_reader(io=io)
        reader.read_spd(0x51, 512, 8)
        self.assertEqual(io.page_writes[-1], 0x03,
                         "SPD page was not restored to what it was")

    def test_read_spd_refuses_a_pmic_address(self):
        reader, io = make_reader()
        with self.assertRaises(ValueError):
            reader.read_spd(0x49, 512, 8)
        self.assertEqual(io.writes, [])

    def test_unknown_controller_offset_is_refused(self):
        reader, io = make_reader()
        with self.assertRaises(ValueError):
            reader.read_byte(0x49, 0x30, controller_offset=0x40)
        self.assertEqual(io.writes, [])

    def test_probe_address_reports_failures_as_absent(self):
        reader, _ = make_reader()
        self.assertFalse(reader.probe_address(0x30))

    def test_read_block_skips_registers_that_fail(self):
        class FlakyIO(FakeIO):
            def inb(self, port):
                if port - FALLBACK_SMBUS_BASE == REG_HOST_DATA0:
                    raise OSError("bus glitch")
                return FakeIO.inb(self, port)

        reader, _ = make_reader(io=FlakyIO())
        self.assertEqual(reader.read_block(0x49, (0x30, 0x31)), {})


class Ddr5PmicDecodeTest(unittest.TestCase):
    def test_confirmed_rails_use_the_vid_registers(self):
        self.assertEqual(
            {key: value[0] for key, value in CONFIRMED_PMIC_RAILS.items()},
            {"dram_vdd": 0x21, "dram_vddq": 0x25, "dram_vpp": 0x27},
        )

    def test_captured_registers_decode_to_the_hwinfo_readings(self):
        # 8200 MT/s: R2Bh = 0x72 puts SWA and SWB into 8-bit mode.
        table = {0x2B: 0x72, 0x21: 0x8C, 0x25: 0x80, 0x27: 0x78}
        rails = ddr5_pmic.decode_rails(lambda register: table[register])
        self.assertAlmostEqual(rails["dram_vdd"], 1.500, places=4)
        self.assertAlmostEqual(rails["dram_vddq"], 1.440, places=4)
        self.assertAlmostEqual(rails["dram_vpp"], 1.800, places=4)

    def test_same_registers_decode_correctly_at_jedec(self):
        # JEDEC 4800: R2Bh = 0x42, so SWA/SWB fall back to the 7-bit decode.
        # The identical VID byte 0x78 means 1.100 V here and 1.400 V in 8-bit
        # mode, which is exactly why the mode register has to be consulted.
        table = {0x2B: 0x42, 0x21: 0x78, 0x25: 0x78, 0x27: 0x78}
        rails = ddr5_pmic.decode_rails(lambda register: table[register])
        self.assertAlmostEqual(rails["dram_vdd"], 1.100, places=4)
        self.assertAlmostEqual(rails["dram_vddq"], 1.100, places=4)
        self.assertAlmostEqual(rails["dram_vpp"], 1.800, places=4)

    def test_vpp_ignores_the_mode_bits(self):
        # SWC stayed 7-bit at both profiles; an 8-bit decode of 0x78 would
        # report 2.100 V and be dropped as out of range.
        for mode in (0x42, 0x72, 0xFF):
            table = {0x2B: mode, 0x21: 0x8C, 0x25: 0x80, 0x27: 0x78}
            rails = ddr5_pmic.decode_rails(lambda register: table[register])
            self.assertAlmostEqual(rails["dram_vpp"], 1.800, places=4)

    def test_out_of_range_rail_is_dropped(self):
        table = {0x2B: 0x72, 0x21: 0xFF, 0x25: 0x80, 0x27: 0x78}
        rails = ddr5_pmic.decode_rails(lambda register: table[register])
        self.assertNotIn("dram_vdd", rails)
        self.assertIn("dram_vddq", rails)

    def test_failed_register_read_yields_no_rails(self):
        def read(register):
            raise OSError("bus glitch")

        self.assertEqual(ddr5_pmic.decode_rails(read), {})

    def test_the_mode_register_alone_settles_the_vid_encoding(self):
        # 0x8C is 1.150 V as 7-bit and 1.500 V as 8-bit. The mode register
        # decides, so no ADC measurement is needed to break the tie.
        table = {0x2B: 0x72, 0x21: 0x8C, 0x25: 0x80, 0x27: 0x78}
        eight_bit = ddr5_pmic.decode_rails(lambda register: table[register])
        table[0x2B] = 0x42
        seven_bit = ddr5_pmic.decode_rails(lambda register: table[register])
        self.assertAlmostEqual(eight_bit["dram_vdd"], 1.500, places=4)
        self.assertAlmostEqual(seven_bit["dram_vdd"], 1.150, places=4)


class DimmTemperatureTest(unittest.TestCase):
    """One SPD hub sensor per module, not one number for the pair."""

    class FakeHubs:
        def __init__(self, temperatures, driver_open=True, device_type=None):
            # {(controller, address): celsius}
            self.temperatures = temperatures
            self._driver_open = driver_open
            # What MR0/MR1 answer. Real hubs say 0x51/0x18; a DDR4 SPD EEPROM
            # at the same address answers with ordinary SPD bytes.
            self.device_type = (
                ddr5_pmic.SPD_HUB_DEVICE_TYPE if device_type is None
                else device_type
            )
            self.reads = []

        def is_driver_open(self):
            return self._driver_open

        def read_byte(self, address, register, controller=0x00):
            self.reads.append((controller, address, register))
            celsius = self.temperatures.get((controller, address))
            if celsius is None:
                raise OSError("no device at 0x%02X" % address)
            identity = ddr5_pmic.SPD_HUB_DEVICE_TYPE_REGISTER
            if register in (identity, identity + 1):
                return self.device_type[register - identity]
            raw = (int(round(celsius / ddr5_pmic.SPD_TEMPERATURE_STEP_C)) & 0x7FF) << 2
            offset = register - ddr5_pmic.SPD_TEMPERATURE_REGISTER
            return (raw >> (8 * offset)) & 0xFF

    def setUp(self):
        ddr5_pmic._SPD_LOCATIONS.update(reader=None, locations=())

    def _read(self, hubs):
        return ddr5_pmic.read_dimm_temperatures(reader_factory=lambda: hubs)

    def test_hub_address_names_the_channel(self):
        self.assertEqual(ddr5_pmic.spd_hub_channel(0x50), "a")
        self.assertEqual(ddr5_pmic.spd_hub_channel(0x51), "a")
        self.assertEqual(ddr5_pmic.spd_hub_channel(0x52), "b")
        self.assertEqual(ddr5_pmic.spd_hub_channel(0x53), "b")
        # Beyond the four slots an AM5 board has, claim nothing.
        self.assertIsNone(ddr5_pmic.spd_hub_channel(0x54))

    def test_both_populated_hubs_are_read(self):
        # The bench layout: 0x51 on channel A, 0x53 on channel B.
        hubs = self.FakeHubs({(0x00, 0x51): 28.75, (0x00, 0x53): 31.25})
        self.assertEqual(self._read(hubs), {"a": 28.75, "b": 31.25})

    def test_a_single_dimm_leaves_the_other_channel_absent(self):
        hubs = self.FakeHubs({(0x00, 0x51): 30.0})
        self.assertEqual(self._read(hubs), {"a": 30.0})

    def test_the_second_slot_of_a_channel_does_not_overwrite_the_first(self):
        hubs = self.FakeHubs({(0x00, 0x50): 25.0, (0x00, 0x51): 40.0})
        self.assertEqual(self._read(hubs), {"a": 25.0})

    def test_a_device_that_is_not_a_hub_is_not_read_as_one(self):
        # A DDR4 module's SPD EEPROM sits at these same addresses and answers
        # a byte read. Its bytes 0x31/0x32 are ordinary SPD content, and here
        # they would decode to 25 C -- inside the band, indistinguishable from
        # a measurement. Only the identity registers tell the two apart.
        eeprom = self.FakeHubs(
            {(0x00, 0x51): 25.0}, device_type=(0x23, 0x13)
        )
        self.assertEqual(self._read(eeprom), {})

    def test_a_hub_identifies_itself_before_its_reading_is_used(self):
        hubs = self.FakeHubs({(0x00, 0x51): 28.0})
        self._read(hubs)
        identity = ddr5_pmic.SPD_HUB_DEVICE_TYPE_REGISTER
        asked = [r for _c, a, r in hubs.reads if a == 0x51]
        self.assertIn(identity, asked)
        self.assertLess(
            asked.index(identity), asked.index(ddr5_pmic.SPD_TEMPERATURE_REGISTER)
        )

    def test_an_out_of_range_reading_is_dropped(self):
        hubs = self.FakeHubs({(0x00, 0x51): 28.0, (0x00, 0x53): 200.0})
        self.assertEqual(self._read(hubs), {"a": 28.0})

    def test_no_driver_reports_nothing(self):
        hubs = self.FakeHubs({(0x00, 0x51): 28.0}, driver_open=False)
        self.assertEqual(self._read(hubs), {})

    def test_a_repeat_read_skips_the_bus_scan(self):
        hubs = self.FakeHubs({(0x00, 0x51): 28.0, (0x00, 0x53): 29.0})
        self._read(hubs)
        probed_first = {(c, a) for c, a, _ in hubs.reads}
        hubs.reads.clear()
        self.assertEqual(self._read(hubs), {"a": 28.0, "b": 29.0})
        probed_again = {(c, a) for c, a, _ in hubs.reads}
        self.assertEqual(probed_again, {(0x00, 0x51), (0x00, 0x53)})
        self.assertLess(len(probed_again), len(probed_first))

    def test_the_single_value_helper_reports_the_warmer_module(self):
        hubs = self.FakeHubs({(0x00, 0x51): 28.0, (0x00, 0x53): 33.0})
        self.assertEqual(
            ddr5_pmic.read_dimm_temperature(reader_factory=lambda: hubs), 33.0
        )

    def test_the_single_value_helper_reports_none_when_no_hub_answers(self):
        hubs = self.FakeHubs({})
        self.assertIsNone(
            ddr5_pmic.read_dimm_temperature(reader_factory=lambda: hubs)
        )


if __name__ == "__main__":
    unittest.main()
