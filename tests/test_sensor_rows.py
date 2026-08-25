"""Cover which Sensors rows each Intel platform installs."""

import unittest
from unittest import mock

from platform_profiles import LGA1700_DDR4, LGA1700_DDR5, LGA1851
from tests.intel_stub import install, restore

intel_timings = None


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


class SensorGroupTest(unittest.TestCase):
    """Temperatures and power are one section, as they are on AM5."""

    def test_every_thermal_row_shares_one_category(self):
        categories = {category for _n, category, _g, _c
                      in intel_timings.SENSOR_ROWS}
        self.assertEqual(
            categories,
            {"Clocks", "Thermal & Power", "Voltages", "Graphics", "Errors"})
        # The temperatures live with the power rather than in a section of
        # their own; the card and the error counter are their own subjects.
        self.assertNotIn("Temperatures", categories)

    def test_every_category_has_a_place_in_the_window(self):
        # main orders the window by SENSOR_GROUP_ORDER, and a category absent
        # from it sorts to the end with no heading of its own.
        from main import SENSOR_GROUP_ORDER

        for _name, category, _getter, _column in intel_timings.SENSOR_ROWS:
            with self.subTest(category=category):
                self.assertIn(category, SENSOR_GROUP_ORDER)

    def test_the_card_is_read_once_a_tick_rather_than_once_a_row(self):
        # Eight rows each opening their own NVML session would pay the init
        # cost eight times a second. The first row refreshes the set and the
        # rest read what it left, so the order of the labels is load-bearing.
        labels = intel_timings.GRAPHICS_SENSOR_LABELS
        self.assertEqual(len(intel_timings.GRAPHICS_SENSOR_ROWS), len(labels))
        refreshing = [
            label for label, row in zip(labels,
                                        intel_timings.GRAPHICS_SENSOR_ROWS)
            if row[2].__defaults__[1]
        ]
        self.assertEqual(refreshing, [labels[0]])

    def test_the_error_counter_is_platform_neutral(self):
        # It reads the Windows event log. Nothing about it is AMD, and it was
        # on that profile alone only because that is where it was written.
        import inspect

        source = inspect.getsource(intel_timings._whea_errors)
        self.assertIn("from whea_errors import error_text", source)
        names = [name for name, _c, _g, _col in intel_timings.ERROR_SENSOR_ROWS]
        self.assertEqual(names, ["WHEA Errors"])

    def test_the_clocks_lead_the_window(self):
        # What the silicon is running at, then what that costs it in heat and
        # power, then the rails feeding it.
        from main import SENSOR_GROUP_ORDER

        self.assertEqual(SENSOR_GROUP_ORDER[0], "Clocks")

    def test_every_clock_row_is_ordered_before_the_rest(self):
        # The window groups by category but keeps declaration order inside a
        # group, so a clock row declared among the voltages would still render
        # under Clocks and be impossible to find in the table.
        categories = [category for _n, category, _g, _c
                      in intel_timings.SENSOR_ROWS]
        clocks = [i for i, c in enumerate(categories) if c == "Clocks"]
        self.assertTrue(clocks)
        self.assertEqual(clocks, list(range(len(clocks))))

    def test_the_section_reads_temperatures_then_power(self):
        # The group is named for that order, and it is the order the AM5
        # block uses: what the parts sit at, then what is being drawn.
        thermal = [name for name, category, _g, _c
                   in intel_timings.SENSOR_ROWS
                   if category == "Thermal & Power"]
        power = [name for name in thermal if name.endswith("Power")]
        temperatures = [name for name in thermal if name.endswith("Temp")]
        self.assertTrue(power and temperatures)
        self.assertLess(max(thermal.index(n) for n in temperatures),
                        min(thermal.index(n) for n in power))

    def test_the_group_name_matches_the_windows_ordering(self):
        # An unlisted category sorts to the end, so a typo here would put the
        # whole section below Voltages rather than above it.
        from main import SENSOR_GROUP_ORDER

        self.assertIn("Thermal & Power", SENSOR_GROUP_ORDER)


class RowLabelTest(unittest.TestCase):
    def test_the_whole_lga1700_socket_calls_the_core_rail_vcore(self):
        # One socket reading one Super I/O. Which DRAM generation is fitted
        # has nothing to do with what the channel is called, and listing only
        # the DDR5 half left the DDR4 board naming a stage it does not have.
        for platform in (LGA1700_DDR5, LGA1700_DDR4):
            with self.subTest(platform=platform):
                self.assertEqual(
                    intel_timings.sensor_row_label(platform, "DLVR Vcore"),
                    "Vcore",
                )

    def test_the_whole_lga1700_socket_calls_the_agent_rail_sa(self):
        for platform in (LGA1700_DDR5, LGA1700_DDR4):
            with self.subTest(platform=platform):
                self.assertEqual(
                    intel_timings.sensor_row_label(platform, "CPU SA (VRM)"),
                    "SA",
                )

    def test_lga1851_keeps_the_declared_name(self):
        # LGA 1851 reads the DLVR output on this channel, and naming that
        # stage is the point there.
        self.assertEqual(
            intel_timings.sensor_row_label(LGA1851, "DLVR Vcore"),
            "DLVR Vcore",
        )

    def test_an_unlisted_row_is_never_renamed(self):
        for name in ("VCCSA", "VDD2", "CPU Temp"):
            with self.subTest(name=name):
                self.assertEqual(
                    intel_timings.sensor_row_label(LGA1700_DDR5, name), name
                )

    def test_every_renamed_row_is_a_real_row(self):
        rows = {name for name, _c, _g, _col in intel_timings.SENSOR_ROWS}
        for platform, labels in intel_timings.SENSOR_ROW_LABELS.items():
            for name in labels:
                with self.subTest(platform=platform, name=name):
                    self.assertIn(name, rows)

    def test_a_renamed_row_is_not_also_dropped(self):
        # Renaming a row this platform does not install would be a rule with
        # nothing to apply to, and a sign the two lists had drifted.
        for platform, labels in intel_timings.SENSOR_ROW_LABELS.items():
            absent = set(intel_timings.absent_sensor_rows(platform, False))
            for name in labels:
                with self.subTest(platform=platform, name=name):
                    self.assertNotIn(name, absent)


class AbsentRowSelectionTest(unittest.TestCase):
    def test_lga1700_ddr5_drops_exactly_the_requested_rows(self):
        absent = intel_timings.absent_sensor_rows(LGA1700_DDR5, False)
        self.assertEqual(set(absent), {
            # no reading reaches this board
            "VCCIO", "CPU VNNAON", "DRAM",
            # reported per module in the Telemetry window
            "DRAM VDD", "DRAM VDDQ", "DRAM VPP",
            "DIMM A Temp", "DIMM B Temp",
            # the row beside it carries the same rail
            "VCCSA",
        })

    def test_the_dimm_temperatures_stay_on_other_platforms(self):
        # DDR4 modules carry their own JC-42.4 sensor and there is no
        # per-module panel there, so the pair is the only reading available.
        for platform in (LGA1700_DDR4, LGA1851):
            absent = intel_timings.absent_sensor_rows(platform, False)
            for name in ("DIMM A Temp", "DIMM B Temp"):
                with self.subTest(platform=platform, name=name):
                    self.assertNotIn(name, absent)

    def test_arrow_lake_keeps_its_own_list(self):
        # Arrow Lake is decided by register layout, not by the platform id, so
        # it wins the branch regardless of what id comes with it.
        absent = intel_timings.absent_sensor_rows(LGA1851, True)
        self.assertEqual(
            absent, intel_timings.ARROW_LAKE_ABSENT_SENSOR_ROWS
        )
        # The DIMM PMIC rails stay on the tab there: on Arrow Lake they are
        # not read yet, which is a blank, not an absence.
        for name in ("DRAM VDD", "DRAM VDDQ", "DRAM VPP"):
            with self.subTest(name=name):
                self.assertNotIn(name, absent)

    def test_ddr4_drops_the_rows_that_generation_does_not_have(self):
        absent = intel_timings.absent_sensor_rows(LGA1700_DDR4, False)
        self.assertEqual(set(absent), {
            # not a rail on a DDR4 board, or on this socket
            "VDD2", "VTT", "VCCIO", "CPU VNNAON",
            # read from the module's PMIC, and a DDR4 module has none
            "DRAM VDD", "DRAM VDDQ", "DRAM VPP",
            # the row beside it carries the same rail
            "VCCSA",
        })

    def test_ddr4_drops_the_duplicate_agent_rail_the_way_ddr5_does(self):
        # VCCSA reads correctly on DDR4 -- 1.201 V against the VRM's 1.200 --
        # and comes off for the one reason that has nothing to do with
        # generation: it is the same rail measured twice.
        for platform in (LGA1700_DDR5, LGA1700_DDR4):
            with self.subTest(platform=platform):
                absent = intel_timings.absent_sensor_rows(platform, False)
                self.assertIn("VCCSA", absent)
                self.assertNotIn("CPU SA (VRM)", absent)

    def test_the_ddr4_board_rails_that_do_read_stay(self):
        absent = set(intel_timings.absent_sensor_rows(LGA1700_DDR4, False))
        for name in ("DLVR Vcore", "CPU SA (VRM)", "VDDQ TX",
                     "CPU AUX", "DRAM"):
            with self.subTest(name=name):
                self.assertNotIn(name, absent)

    def test_ddr4_drops_the_whole_pmic_block_together(self):
        # DRAM VPP printed 1.500 V here while its two neighbours were blank,
        # which read as "this one works". It did not: the sweep for a PMIC
        # found a device at 0x4F that is not one. The three stand or fall
        # together because they have one source.
        absent = set(intel_timings.absent_sensor_rows(LGA1700_DDR4, False))
        pmic = {"DRAM VDD", "DRAM VDDQ", "DRAM VPP"}
        self.assertTrue(pmic <= absent or not (pmic & absent))

    def test_every_dropped_name_is_a_real_row(self):
        # A rename that missed one of these lists would otherwise leave an
        # entry that silently drops nothing.
        rows = {name for name, _c, _g, _col in intel_timings.SENSOR_ROWS}
        listed = (
            intel_timings.ARROW_LAKE_ABSENT_SENSOR_ROWS
            + intel_timings.LGA1700_DDR5_ABSENT_SENSOR_ROWS
            + intel_timings.LGA1700_DDR5_PER_DIMM_SENSOR_ROWS
            + intel_timings.LGA1700_DDR5_DUPLICATE_SENSOR_ROWS
            + intel_timings.LGA1700_DDR4_ABSENT_SENSOR_ROWS
        )
        for name in listed:
            with self.subTest(name=name):
                self.assertIn(name, rows)

    def test_the_three_ddr5_lists_say_different_things(self):
        # "no such reading here", "reported per DIMM instead" and "the row
        # beside it carries it" are three different statements. Collapsing any
        # two would lose that, so nothing may appear in more than one.
        lists = (
            intel_timings.LGA1700_DDR5_ABSENT_SENSOR_ROWS,
            intel_timings.LGA1700_DDR5_PER_DIMM_SENSOR_ROWS,
            intel_timings.LGA1700_DDR5_DUPLICATE_SENSOR_ROWS,
        )
        names = [name for group in lists for name in group]
        self.assertEqual(len(names), len(set(names)))

    def test_the_rails_that_do_read_here_are_kept(self):
        absent = set(intel_timings.absent_sensor_rows(LGA1700_DDR5, False))
        for name in ("DLVR Vcore", "CPU SA (VRM)", "VDDQ TX",
                     "CPU AUX", "VDD2", "CPU Temp", "PCH Temp"):
            with self.subTest(name=name):
                self.assertNotIn(name, absent)

    def test_the_kept_sa_row_is_the_one_that_gets_renamed(self):
        # Dropping VCCSA and renaming the other row only reads correctly if
        # the survivor is the one carrying the new name.
        absent = set(intel_timings.absent_sensor_rows(LGA1700_DDR5, False))
        self.assertIn("VCCSA", absent)
        self.assertNotIn("CPU SA (VRM)", absent)
        self.assertEqual(
            intel_timings.sensor_row_label(LGA1700_DDR5, "CPU SA (VRM)"), "SA"
        )


class BoardAbsentRowTest(unittest.TestCase):
    """Rows a board does not wire, as opposed to ones its silicon lacks."""

    def test_asus_drops_the_socket_channel(self):
        # The NCT6798D on the Z790 APEX has no socket channel. The row read
        # blank on every tick, and blank means "not read yet" here.
        for spelling in ("ASUSTeK COMPUTER INC.", "ASUS", "asustek"):
            with self.subTest(name=spelling):
                self.assertEqual(
                    intel_timings.board_absent_sensor_rows(spelling),
                    ("CPU Socket Temp",))

    def test_another_maker_keeps_it(self):
        # Dropped for the board that does not wire it, not for everyone.
        for spelling in ("Micro-Star International", "Gigabyte", "ASRock",
                         "", None):
            with self.subTest(name=spelling):
                self.assertEqual(
                    intel_timings.board_absent_sensor_rows(spelling), ())

    def test_the_board_list_joins_the_platform_list(self):
        # Both reasons apply at once: a row can be absent from the silicon
        # and unwired on the board, and dropping one list when the other
        # applies would put a blank row back.
        from platform_profiles import LGA1700_DDR4, LGA1700_DDR5, LGA1851

        for platform, arrow_lake in ((LGA1700_DDR5, False),
                                     (LGA1700_DDR4, False),
                                     (LGA1851, True)):
            with self.subTest(platform=platform):
                absent = intel_timings.absent_sensor_rows(
                    platform, arrow_lake, "ASUSTeK COMPUTER INC.")
                self.assertIn("CPU Socket Temp", absent)
                plain = intel_timings.absent_sensor_rows(platform, arrow_lake)
                for name in plain:
                    self.assertIn(name, absent)


class CoreMaxTempTest(unittest.TestCase):
    """The die reading, and the range it refuses to believe."""

    def _with_raw(self, raw):
        return mock.patch.object(intel_timings, "read_physical_memory_int",
                                 return_value=raw)

    def test_a_plausible_reading_is_shown_in_celsius(self):
        for raw, shown in ((33, "33.0 °C"), (77, "77.0 °C"),
                           (100, "100.0 °C")):
            with self.subTest(raw=raw), self._with_raw(raw):
                self.assertEqual(intel_timings.get_core_max_temp(), shown)

    def test_nothing_outside_the_range_is_believed(self):
        # Zero is the register unread rather than a die at freezing, and a
        # 14900KS throttles at 100 -- 200 is a register holding something
        # else. Either way the row says nothing rather than a wrong number.
        for raw in (0, 1, 4, 126, 200, 0xFFFFFFFF, None):
            with self.subTest(raw=raw), self._with_raw(raw):
                self.assertIsNone(intel_timings.get_core_max_temp())

    def test_it_reads_the_die_not_the_board_channel(self):
        # CPU Temp is the Super I/O's channel and sits flat while this spikes.
        # Two rows, two sources, and neither should start reading the other.
        self.assertEqual(intel_timings.CORE_MAX_TEMP_OFFSET, 0x5978)
        rows = {name: getter for name, _c, getter, _col
                in intel_timings.SENSOR_ROWS}
        self.assertIs(rows["Core Max"], intel_timings.get_core_max_temp)
        self.assertIsNot(rows["CPU Temp"], intel_timings.get_core_max_temp)

    def test_the_mirror_is_recorded_but_not_read_twice(self):
        # 0x597C holds the same value -- 198 of 200 idle reads agreed, and 60
        # of 60 under a single-core load. One row, not two.
        self.assertEqual(intel_timings.CORE_MAX_TEMP_MIRROR, 0x597C)
        names = [name for name, _c, _g, _col in intel_timings.SENSOR_ROWS]
        self.assertEqual(names.count("Core Max"), 1)


if __name__ == "__main__":
    unittest.main()
