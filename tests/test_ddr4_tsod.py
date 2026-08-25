"""Cover the JC-42.4 decode, the channel mapping and the reading cache."""

import unittest

import ddr4_tsod
from ddr4_tsod import (
    AMBIENT_TEMPERATURE_REGISTER,
    channel_for_address,
    decode_temperature,
    read_dimm_temperatures,
    temperature_text,
)


class _FakeReader:
    """Answers for a chosen set of sensor addresses, in wire order."""

    def __init__(self, words):
        self.words = words
        self.reads = []

    def responding_addresses(self, register=AMBIENT_TEMPERATURE_REGISTER):
        return tuple(sorted(self.words))

    def read_word_bytes(self, address, register):
        self.reads.append((address, register))
        if address not in self.words:
            raise OSError("no sensor at 0x%02X" % address)
        return self.words[address]


class DecodeTest(unittest.TestCase):
    """Values captured from the Z790 target, plus the boundary cases."""

    def test_the_reading_captured_on_channel_a(self):
        # 0xC1E8: alarm flags in the top bits, 0x1E8 = 488 sixteenths.
        self.assertEqual(decode_temperature(0xC1, 0xE8), 30.5)

    def test_the_reading_captured_on_channel_b(self):
        self.assertEqual(decode_temperature(0xC2, 0x00), 32.0)

    def test_alarm_flags_are_not_part_of_the_number(self):
        # Same temperature, every alarm flag combination.
        for flags in (0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0):
            with self.subTest(flags=flags):
                self.assertEqual(decode_temperature(0x01 | flags, 0xE8), 30.5)

    def test_a_sixteenth_of_a_degree_resolves(self):
        self.assertEqual(decode_temperature(0x00, 0x01), 0.0625)

    def test_a_negative_temperature_decodes(self):
        # -0.25 C is 0x1FFC in the 13-bit two's complement field.
        self.assertEqual(decode_temperature(0x1F, 0xFC), -0.25)

    def test_minus_one_degree_decodes(self):
        self.assertEqual(decode_temperature(0x1F, 0xF0), -1.0)

    def test_zero_decodes(self):
        self.assertEqual(decode_temperature(0x00, 0x00), 0.0)

    def test_a_decode_below_the_parts_range_is_dropped(self):
        # 0x1000 is the sign bit alone: -256 C, which no sensor reports.
        self.assertIsNone(decode_temperature(0x10, 0x00))

    def test_a_decode_above_the_parts_range_is_dropped(self):
        # 0x0FFF is +255.9375 C.
        self.assertIsNone(decode_temperature(0x0F, 0xFF))

    def test_unreadable_bytes_are_dropped(self):
        self.assertIsNone(decode_temperature(None, 0x00))
        self.assertIsNone(decode_temperature("C1", "E8"))

    def test_byte_order_is_most_significant_first(self):
        # Swapping the captured bytes must not yield the same temperature.
        self.assertNotEqual(
            decode_temperature(0xC1, 0xE8), decode_temperature(0xE8, 0xC1)
        )


class ChannelMappingTest(unittest.TestCase):
    def test_the_first_two_slots_are_channel_a(self):
        self.assertEqual(channel_for_address(0x18), "a")
        self.assertEqual(channel_for_address(0x19), "a")

    def test_the_next_two_slots_are_channel_b(self):
        self.assertEqual(channel_for_address(0x1A), "b")
        self.assertEqual(channel_for_address(0x1B), "b")

    def test_addresses_beyond_the_mapped_slots_are_unmapped(self):
        for address in (0x1C, 0x1D, 0x1E, 0x1F):
            with self.subTest(address=address):
                self.assertIsNone(channel_for_address(address))

    def test_an_address_below_the_range_is_unmapped(self):
        self.assertIsNone(channel_for_address(0x17))


class ReadTest(unittest.TestCase):
    def setUp(self):
        ddr4_tsod.reset_cache()
        self.addCleanup(ddr4_tsod.reset_cache)

    def test_the_captured_layout_maps_to_both_channels(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8), 0x1B: (0xC2, 0x00)})
        self.assertEqual(
            read_dimm_temperatures(reader_factory=lambda: reader),
            {"a": 30.5, "b": 32.0},
        )

    def test_the_hotter_module_in_a_channel_wins(self):
        reader = _FakeReader({0x18: (0xC1, 0xE8), 0x19: (0xC2, 0x00)})
        self.assertEqual(
            read_dimm_temperatures(reader_factory=lambda: reader), {"a": 32.0}
        )

    def test_a_board_with_no_sensors_reports_nothing(self):
        reader = _FakeReader({})
        self.assertEqual(read_dimm_temperatures(reader_factory=lambda: reader), {})

    def test_an_implausible_reading_is_left_out(self):
        reader = _FakeReader({0x19: (0x0F, 0xFF)})
        self.assertEqual(read_dimm_temperatures(reader_factory=lambda: reader), {})

    def test_a_failure_to_reach_the_bus_reports_nothing(self):
        def explode():
            raise OSError("no controller")

        self.assertEqual(read_dimm_temperatures(reader_factory=explode), {})

    def test_formatting_carries_one_decimal_and_a_unit(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        self.assertEqual(
            temperature_text("a", reader_factory=lambda: reader), "30.5 °C"
        )

    def test_formatting_an_absent_channel_reports_nothing(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        self.assertIsNone(temperature_text("b", reader_factory=lambda: reader))


class PeakTest(unittest.TestCase):
    """The peak is what a memory test is judged on, not the current reading."""

    def setUp(self):
        ddr4_tsod.reset_cache()
        self.addCleanup(ddr4_tsod.reset_cache)

    def test_the_peak_starts_empty(self):
        self.assertEqual(ddr4_tsod.peak_temperatures(), {})

    def test_the_first_reading_becomes_the_peak(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: reader)
        self.assertEqual(ddr4_tsod.peak_temperatures(), {"a": 30.5})

    def test_a_hotter_reading_raises_the_peak(self):
        hot = _FakeReader({0x19: (0xC2, 0x00)})
        cool = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: cool)
        read_dimm_temperatures(reader_factory=lambda: hot)
        self.assertEqual(ddr4_tsod.peak_temperatures()["a"], 32.0)

    def test_a_cooler_reading_does_not_lower_the_peak(self):
        hot = _FakeReader({0x19: (0xC2, 0x00)})
        cool = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: hot)
        read_dimm_temperatures(reader_factory=lambda: cool)
        self.assertEqual(ddr4_tsod.peak_temperatures()["a"], 32.0)

    def test_each_channel_keeps_its_own_peak(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8), 0x1B: (0xC2, 0x00)})
        read_dimm_temperatures(reader_factory=lambda: reader)
        self.assertEqual(
            ddr4_tsod.peak_temperatures(), {"a": 30.5, "b": 32.0}
        )

    def test_the_row_shows_the_reading_and_not_the_peak(self):
        # It used to append "(max 32.0)". These rows are in the telemetry
        # window now, which has a Max column of its own, so the suffix was
        # both redundant and nineteen characters wide in a column sized for
        # seven -- it overflowed across the Min and Max cells beside it.
        hot = _FakeReader({0x19: (0xC2, 0x00)})
        cool = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: hot)
        self.assertEqual(
            temperature_text("a", reader_factory=lambda: cool), "30.5 °C"
        )

    def test_the_row_is_the_same_width_whatever_the_peak_is(self):
        # The overflow only appeared once a peak had been recorded, so the
        # row was fine until the kit warmed up and then broke the table.
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        plain = temperature_text("a", reader_factory=lambda: reader)
        ddr4_tsod._peaks["a"] = 99.9
        self.assertEqual(
            temperature_text("a", reader_factory=lambda: reader), plain
        )

    def test_the_peak_is_still_tracked_for_callers_that_want_it(self):
        # Dropping it from the row does not drop it from the module: this one
        # spans the process and survives Reset Stats, which the window's
        # maximum does not.
        hot = _FakeReader({0x19: (0xC2, 0x00)})
        cool = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: hot)
        read_dimm_temperatures(reader_factory=lambda: cool)
        self.assertEqual(ddr4_tsod.peak_temperatures()["a"], 32.0)

    def test_resetting_clears_the_peak(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: reader)
        ddr4_tsod.reset_cache()
        self.assertEqual(ddr4_tsod.peak_temperatures(), {})

    def test_the_peak_is_a_copy_that_callers_cannot_corrupt(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: reader)
        ddr4_tsod.peak_temperatures()["a"] = 999.0
        self.assertEqual(ddr4_tsod.peak_temperatures()["a"], 30.5)


class CacheTest(unittest.TestCase):
    """A row must not cost a bus transaction every time it is drawn."""

    def setUp(self):
        ddr4_tsod.reset_cache()
        self.addCleanup(ddr4_tsod.reset_cache)

    def test_a_repeat_read_inside_the_window_does_not_touch_the_bus(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        clock = iter([0.0, 0.0, 0.1, 0.2])
        ddr4_tsod._reader = reader
        ddr4_tsod._addresses = (0x19,)

        first = read_dimm_temperatures(monotonic=lambda: next(clock))
        self.assertEqual(first, {"a": 30.5})
        before = len(reader.reads)

        second = read_dimm_temperatures(monotonic=lambda: next(clock))
        self.assertEqual(second, {"a": 30.5})
        self.assertEqual(len(reader.reads), before)

    def test_the_bus_is_read_again_once_the_window_passes(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        clock = iter([0.0, 0.0, 10.0, 10.0])
        ddr4_tsod._reader = reader
        ddr4_tsod._addresses = (0x19,)

        read_dimm_temperatures(monotonic=lambda: next(clock))
        before = len(reader.reads)
        read_dimm_temperatures(monotonic=lambda: next(clock))
        self.assertGreater(len(reader.reads), before)

    def test_an_injected_reader_bypasses_the_cache(self):
        reader = _FakeReader({0x19: (0xC1, 0xE8)})
        read_dimm_temperatures(reader_factory=lambda: reader)
        before = len(reader.reads)
        read_dimm_temperatures(reader_factory=lambda: reader)
        self.assertGreater(len(reader.reads), before)


if __name__ == "__main__":
    unittest.main()
