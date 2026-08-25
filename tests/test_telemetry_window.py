import inspect
import unittest
from unittest import mock

import dimm_telemetry_window as w
from display_values import WINDOWED_TABS, select_tab_names


class ParseReadingTest(unittest.TestCase):
    """A displayed reading is text; the statistics need the number in it."""

    def test_a_plain_value_gives_its_number_unit_and_precision(self):
        self.assertEqual(w.parse_reading("1.244 V"), (1.244, "V", 3))
        self.assertEqual(w.parse_reading("42.1 °C"), (42.1, "°C", 1))
        self.assertEqual(w.parse_reading("2.00 x"), (2.0, "x", 2))

    def test_a_value_paired_with_its_limit_tracks_the_value(self):
        # "38.7 / 300.0 W" is the reading and the configured limit. Only the
        # first moves, so only the first is worth a minimum and a maximum.
        self.assertEqual(w.parse_reading("38.7 / 300.0 W"), (38.7, "W", 1))

    def test_a_unit_is_never_read_as_part_of_a_second_number(self):
        self.assertEqual(w.parse_reading("117 ns / 95 ns"), (117.0, "ns", 0))

    def test_a_reading_with_no_number_keeps_its_place(self):
        for text in ("Off", "Hi-Z", "—", "", None):
            with self.subTest(text=text):
                self.assertEqual(w.parse_reading(text), (None, "", 0))

    def test_a_negative_reading_is_a_number(self):
        self.assertEqual(w.parse_reading("-0.5 V"), (-0.5, "V", 1))


class StatisticTest(unittest.TestCase):
    def test_it_tracks_the_extremes_and_the_mean(self):
        statistic = w.Statistic()
        for value in (1.5, 1.515, 1.5):
            statistic.add(value)
        self.assertEqual(statistic.current, 1.5)
        self.assertEqual(statistic.minimum, 1.5)
        self.assertEqual(statistic.maximum, 1.515)
        self.assertAlmostEqual(statistic.average, 1.505)

    def test_a_missing_reading_does_not_count_as_zero(self):
        statistic = w.Statistic()
        statistic.add(1.5)
        statistic.add(None)
        self.assertEqual(statistic.minimum, 1.5)
        self.assertEqual(statistic.count, 1)

    def test_nothing_read_has_no_statistics(self):
        statistic = w.Statistic()
        self.assertIsNone(statistic.average)
        self.assertIsNone(statistic.minimum)

    def test_reset_clears_everything(self):
        statistic = w.Statistic()
        statistic.add(2.0)
        statistic.reset()
        self.assertIsNone(statistic.current)
        self.assertIsNone(statistic.maximum)
        self.assertEqual(statistic.count, 0)


class FormattingTest(unittest.TestCase):
    def test_values_carry_their_unit_and_precision(self):
        self.assertEqual(w.format_value(1.4405, "V", 3), "1.440 V")
        self.assertEqual(w.format_value(None, "V", 3), "—")

    def test_the_timer_reads_as_a_clock(self):
        self.assertEqual(w.format_elapsed(0), "00:00:00")
        self.assertEqual(w.format_elapsed(3725), "01:02:05")
        self.assertEqual(w.format_elapsed(-5), "00:00:00")


class PanelTest(unittest.TestCase):
    ENTRY = {"channel": "a", "pmic_address": 0x49,
             "vendor": "Richtek Power", "revision": "2.1"}
    MODULE = {"slot": "A2", "capacity_gb": 16, "rank_numeric": "1R",
              "ic": "SK hynix A-die", "part_number": "F5-6000J2636G16G",
              "spd_vendor": "G.Skill"}

    def test_the_heading_names_the_module_and_its_pmic(self):
        self.assertEqual(
            w.panel_title(0, self.ENTRY, self.MODULE),
            "DIMM 0  |  A2  |  PMIC 0x49",
        )

    def test_an_unnamed_slot_falls_back_to_the_channel(self):
        self.assertIn("A", w.panel_title(0, self.ENTRY, {"slot": None}))

    def test_the_identity_lines_carry_what_was_read(self):
        left, right = w.panel_identity(self.ENTRY, self.MODULE)
        self.assertEqual(left[:3], [
            ("IC Manufacturer", "SK hynix"),
            ("DRAM Die", "A-die"),
            ("Rank", "1R"),
        ])
        self.assertEqual(right, [
            ("DRAM Part Number", "F5-6000J2636G16G"),
            ("Module Manufacturer", "G.Skill"),
            ("Capacity", "16384 MB"),
            ("PMIC", "Richtek Power rev 2.1"),
        ])

    def test_the_sides_are_padded_to_the_same_length(self):
        # They are drawn in pairs; a short side must not cut the long one off.
        left, right = w.panel_identity(self.ENTRY, self.MODULE)
        self.assertEqual(len(left), len(right))
        self.assertEqual(left[-1], ("", ""))

    def test_the_module_vendor_is_not_labelled_as_the_dram_maker(self):
        # G.Skill sells the stick; SK hynix made the chips on it. Two labels,
        # because they are two different companies.
        left, right = w.panel_identity(self.ENTRY, self.MODULE)
        self.assertEqual(dict(right)["Module Manufacturer"], "G.Skill")
        self.assertEqual(dict(left)["IC Manufacturer"], "SK hynix")

    def test_a_module_that_read_nothing_still_renders(self):
        left, right = w.panel_identity({"channel": "b"}, None)
        self.assertEqual(left[0], ("IC Manufacturer", "—"))
        self.assertEqual(dict(right)["Capacity"], "—")
        self.assertEqual(dict(right)["PMIC"], "—")
        self.assertEqual(dict(right)["DRAM Part Number"], "—")


class ParameterTest(unittest.TestCase):
    def test_only_the_total_power_is_shown(self):
        # The three per-rail figures the total is made of are still decoded
        # and still available to a caller; they are not worth a row each,
        # because VDD carries nearly all of a DDR5 module's draw and the
        # split reads as the total, a small number and a rounding error.
        keys = [key for key, _label, _unit, _decimals in w.PARAMETERS]
        self.assertIn("power_w", keys)
        for key in ("power_swa_w", "power_swb_w", "power_swc_w"):
            self.assertNotIn(key, keys)

    def test_the_per_rail_powers_are_still_decoded(self):
        # Dropping the rows must not drop the readings: the total is taken
        # from the same decode, and a caller asking for a rail still gets it.
        self.assertEqual(
            w.reading_value({"power_swb_w": 0.135}, "power_swb_w"), 0.135
        )

    def test_the_power_row_is_watts_not_millivolts(self):
        for key in ("power_swa_w", "power_swb_w", "power_swc_w", "power_w"):
            self.assertNotIn(key, w.MILLIVOLT_KEYS)


class ReadingValueTest(unittest.TestCase):
    def test_millivolt_readings_are_shown_in_volts(self):
        self.assertEqual(w.reading_value({"vdd": 1515}, "vdd"), 1.515)

    def test_other_readings_pass_through(self):
        self.assertEqual(w.reading_value({"hub_temp_c": 27.75}, "hub_temp_c"),
                         27.75)
        self.assertEqual(w.reading_value({"power_w": 0.242}, "power_w"), 0.242)

    def test_a_missing_reading_is_absent_not_zero(self):
        self.assertIsNone(w.reading_value({}, "vdd"))


class SensorTabTest(unittest.TestCase):
    def test_the_sensors_tab_is_a_window_now(self):
        rows = [
            {"Tab": "Timings"}, {"Tab": "System Info"}, {"Tab": "Sensors"},
        ]
        self.assertIn("Sensors", WINDOWED_TABS)
        self.assertNotIn("Sensors", select_tab_names(rows))
        self.assertEqual(select_tab_names(rows),
                         ["Summary", "System Info", "Timings"])

    def test_the_other_tabs_are_untouched(self):
        rows = [{"Tab": name} for name in
                ("Timings", "Skew", "Jedec", "RTL", "System Info")]
        self.assertEqual(
            select_tab_names(rows),
            ["Summary", "System Info", "Timings", "Skew", "Jedec", "RTL"],
        )


class BandTest(unittest.TestCase):
    """Rows alternate, and a caller that never asked for banding gets none."""

    class _Window:
        _theme = {"band": ("light", "dark")}

    class _OldWindow:
        _theme = {}

    def test_rows_alternate_starting_unshaded(self):
        band = w.DimmTelemetryWindow._band
        self.assertEqual(band(self._Window(), 0), "transparent")
        self.assertEqual(band(self._Window(), 1), ("light", "dark"))
        self.assertEqual(band(self._Window(), 2), "transparent")

    def test_a_theme_with_no_band_colour_stays_plain(self):
        # The window takes its palette from whoever opens it, so a caller
        # built before banding existed has to keep working.
        band = w.DimmTelemetryWindow._band
        self.assertEqual(band(self._OldWindow(), 1), "transparent")


class FakeWidget:
    """Records what a row's labels were told, and whether they are gridded."""

    def __init__(self):
        self.fg_color = None
        self.gridded = True

    def configure(self, **kwargs):
        if "fg_color" in kwargs:
            self.fg_color = kwargs["fg_color"]

    def grid(self):
        self.gridded = True

    def grid_remove(self):
        self.gridded = False


class CollapsibleRowTest(unittest.TestCase):
    """Rows folded under a parent, and what the banding does around them."""

    BAND = ("light", "dark")

    class _Window:
        _theme = {"band": ("light", "dark")}
        # The real banding rule, so this describes the window's own
        # alternation rather than a second copy of it.
        _band = w.DimmTelemetryWindow._band

    def _window(self, expanded=None):
        window = self._Window()
        window._expanded = expanded or {}
        return window

    def _records(self):
        def record(parent=None):
            return {"filler": FakeWidget(), "cells": [FakeWidget()],
                    "parent": parent}

        return [
            record(),                       # Bus Clock
            record(),                       # Core Clock (avg)
            record(),                       # Core Effective Clock, the parent
            record("Core Effective Clock"),  # CPU 0
            record("Core Effective Clock"),  # CPU 1
            record(),                       # Ring/LLC Clock
        ]

    def _stripes(self, records, window):
        w.DimmTelemetryWindow._restripe(window, records)
        return [
            record["filler"].fg_color for record in records
            if not (record["parent"]
                    and not window._expanded.get(record["parent"]))
        ]

    def test_a_hidden_group_does_not_break_the_alternation(self):
        # Banding by position in the table counts rows nobody can see, so
        # closing a group of two left the row after it repeating the shade of
        # the row before.
        window = self._window()
        self.assertEqual(
            self._stripes(self._records(), window),
            ["transparent", self.BAND, "transparent", self.BAND],
        )

    def test_an_open_group_bands_its_children_in_sequence(self):
        window = self._window({"Core Effective Clock": True})
        self.assertEqual(
            self._stripes(self._records(), window),
            ["transparent", self.BAND, "transparent", self.BAND,
             "transparent", self.BAND],
        )

    def test_a_hidden_row_takes_its_filler_with_it(self):
        # The filler spans the whole row to carry the tint past the last
        # value. Left gridded while the row is hidden it holds the row's
        # height open, which put a block of empty table under the parent.
        records = self._records()
        child = records[3]
        w.DimmTelemetryWindow._hide(child)
        self.assertFalse(child["filler"].gridded)
        self.assertFalse(any(cell.gridded for cell in child["cells"]))

    def test_showing_a_row_restores_every_part_of_it(self):
        records = self._records()
        child = records[3]
        w.DimmTelemetryWindow._hide(child)
        w.DimmTelemetryWindow._show(child)
        self.assertTrue(child["filler"].gridded)
        self.assertTrue(all(cell.gridded for cell in child["cells"]))

    def test_the_marks_say_which_way_the_row_opens(self):
        self.assertNotEqual(w.COLLAPSED_MARK, w.EXPANDED_MARK)
        self.assertTrue(w.CHILD_INDENT.strip() == "")


class PollSchedulingTest(unittest.TestCase):
    """A tick that overruns its interval is skipped, not queued."""

    class _Window:
        def __init__(self, reading):
            self._reading = reading
            self._poll_ms = 1000
            self._after_id = None
            self.scheduled = []
            self.started = 0

        def _poll(self):
            raise AssertionError("rescheduling should not re-enter")

        def _read_tick(self):
            raise AssertionError("the worker should not run inline")

        def after(self, delay, callback, *args):
            self.scheduled.append(delay)
            return "after-id"

    def poll(self, window):
        with mock.patch.object(w.threading, "Thread") as thread:
            w.DimmTelemetryWindow._poll(window)
            window.started = thread.call_count

    def test_a_free_window_starts_a_reader(self):
        window = self._Window(reading=False)
        self.poll(window)
        self.assertEqual(window.started, 1)
        self.assertTrue(window._reading)
        # The next tick is booked by the apply step, not here, or an
        # overrunning read would have two timers racing it.
        self.assertEqual(window.scheduled, [])

    def test_a_busy_window_waits_a_tick_instead_of_stacking_up(self):
        window = self._Window(reading=True)
        self.poll(window)
        self.assertEqual(window.started, 0)
        self.assertEqual(window.scheduled, [1000])

    def test_the_reads_do_not_touch_a_widget(self):
        # Tk is not thread-safe, so the worker returns values and the UI
        # thread writes them; _read_sensors must only call the readers.
        window = mock.Mock()
        window._sensor_groups = [
            ("Clocks", [("Bus Clock", lambda: "100 MHz", None)]),
            ("Voltages", [("VDDCR_VDD", lambda: (_ for _ in ()).throw(OSError),
                           None)]),
        ]
        readings = w.DimmTelemetryWindow._read_sensors(window)
        self.assertEqual(readings, [
            (("sensor", "Clocks", "Bus Clock"), "100 MHz"),
            (("sensor", "Voltages", "VDDCR_VDD"), "—"),
        ])



class TrailingGroupTest(unittest.TestCase):
    """Errors belongs under the DIMM panels, not between them and the rails."""

    def test_the_groups_that_sink_are_graphics_then_errors(self):
        # Order matters: they are re-packed in this order, so Errors lands
        # last, which is where a counter that should stay zero belongs.
        self.assertEqual(w.TRAILING_GROUPS, ("Graphics", "Errors"))

    def test_a_trailing_group_is_re_packed_after_the_panels(self):
        panel = mock.Mock()
        window = mock.Mock(_group_panels={"Errors": panel})
        w.DimmTelemetryWindow._sink_trailing_groups(window)
        # Forgotten and packed again: Tk packs in call order, so this moves
        # the frame to the end without rebuilding the rows or losing the
        # statistics they have already collected.
        panel.pack_forget.assert_called_once()
        panel.pack.assert_called_once()

    def test_a_group_that_was_never_built_is_skipped(self):
        window = mock.Mock(_group_panels={})
        w.DimmTelemetryWindow._sink_trailing_groups(window)

    def test_a_frame_that_will_not_re_pack_does_not_stop_the_others(self):
        broken = mock.Mock()
        broken.pack_forget.side_effect = RuntimeError("gone")
        window = mock.Mock(_group_panels={"Errors": broken})
        w.DimmTelemetryWindow._sink_trailing_groups(window)


class FastSampledRowTest(unittest.TestCase):
    """Rows read between ticks, so their maximum is the run's maximum.

    Measured on the bench: with a bursty load -- 300ms on, 1.2s off, which is
    the shape a memory test has -- sampling every 200ms saw a peak of 75 C
    where sampling every second saw 72. That 3 C is the same gap this tool's
    Core Max maximum showed against HWiNFO over a TestMem5 run, 58 against
    61, while the minimum and average agreed exactly. Under steady load both
    rates agree, which is why the fix is about peaks and not accuracy.
    """

    def test_core_max_is_sampled_between_ticks(self):
        self.assertIn("Core Max", w.FAST_SAMPLE_LABELS)

    def test_it_samples_several_times_a_tick(self):
        # No point being "fast" at a rate the tick already covers.
        self.assertLess(w.FAST_SAMPLE_MS, 1000)
        self.assertGreaterEqual(w.FAST_SAMPLE_MS, 50)

    def test_only_cheap_rows_are_listed(self):
        # These are read on the UI thread, unlike the full tick. A row that
        # blocks -- a DIMM's ADC needs 12ms a channel -- would stall the
        # window five times a second.
        self.assertEqual(w.FAST_SAMPLE_LABELS,
                         frozenset({"Core Max"}))

    def test_a_fast_row_is_not_also_counted_on_the_tick(self):
        # Both would weight the tick's one sample like the five between it,
        # pulling the average toward whatever that sample happened to catch.
        source = inspect.getsource(w.DimmTelemetryWindow
                                   ._apply_sensors)
        self.assertIn("if key not in self._fast_keys:", source)
        feed = inspect.getsource(w.DimmTelemetryWindow
                                 ._fast_sample)
        self.assertIn("statistic", feed)

    def test_double_counting_would_move_the_average(self):
        # Why the guard above exists, in numbers: the same readings counted
        # twice at one rate and once at another do not average the same.
        fed_once = w.Statistic()
        fed_twice = w.Statistic()
        for value in (40, 40, 40, 40, 80):     # four quiet samples, one peak
            fed_once.add(value)
            fed_twice.add(value)
        fed_twice.add(80)                       # the tick catching the peak
        self.assertEqual(fed_once.maximum, fed_twice.maximum)
        self.assertNotEqual(round(fed_once.average, 3),
                            round(fed_twice.average, 3))

    def test_both_timers_are_cancelled_on_close(self):
        # A live after() into a destroyed window is a Tcl error on the way
        # out, and the fast one fires five times as often.
        source = inspect.getsource(w.DimmTelemetryWindow
                                   .close)
        self.assertIn("_after_id", source)
        self.assertIn("_fast_after_id", source)


if __name__ == "__main__":
    unittest.main()
