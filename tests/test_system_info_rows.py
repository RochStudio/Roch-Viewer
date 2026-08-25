"""Cover the System Info identity and per-module rows."""

import contextlib
import unittest
from unittest import mock

from display_values import resolve_display_value
from tests.intel_stub import install, restore

intel_timings = None

NEW_ROWS = (
    "Platform", "OS", "GPU",
    "DIMM Size", "Rank", "DRAM Manufacturer", "DRAM Die",
)


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


def system_info_rows():
    return [
        row for row in intel_timings.TIMINGS
        if row.get("Tab") == "System Info"
    ]


def row_names():
    return [row.get("name") for row in system_info_rows()]


def value_of(name):
    row = next(r for r in system_info_rows() if r.get("name") == name)
    return resolve_display_value(row["value"])



@contextlib.contextmanager
def cpu_model(model, family=6):
    """Report a chosen CPUID family/model, with the identity caches cleared."""
    intel_timings._clear_identity_caches()
    try:
        with mock.patch.object(intel_timings, "_cpu_family_model",
                               return_value=(family, model)):
            yield
    finally:
        intel_timings._clear_identity_caches()


@contextlib.contextmanager
def os_parts(revision, display_version):
    """Report chosen registry halves of the OS string, uncached."""
    intel_timings._clear_identity_caches()
    try:
        with mock.patch.object(intel_timings, "get_windows_update_revision",
                               return_value=revision),                 mock.patch.object(intel_timings, "get_windows_display_version",
                                  return_value=display_version):
            yield
    finally:
        intel_timings._clear_identity_caches()


class PresenceTest(unittest.TestCase):
    def test_every_new_row_is_on_the_tab_exactly_once(self):
        names = row_names()
        for name in NEW_ROWS:
            with self.subTest(name=name):
                self.assertEqual(names.count(name), 1)

    def test_the_capacity_row_was_already_there_and_was_not_duplicated(self):
        self.assertEqual(row_names().count("Memory Capacity"), 1)

    def test_the_platform_row_reuses_the_resolved_classification(self):
        # detect_current_platform opens a WMI connection and runs three
        # queries -- 1.08 seconds on the bench. The Advanced window reads
        # every row on a timer, so this row going to WMI made a pass over the
        # table cost a second on its own.
        import sys

        module = intel_timings
        saved = module.detect_current_platform
        self.addCleanup(
            lambda: setattr(module, "detect_current_platform", saved))
        calls = []

        def refuse():
            calls.append(1)
            raise AssertionError("re-probed the platform over WMI")

        module.detect_current_platform = refuse
        timings = sys.modules.get("timings")
        if getattr(timings, "ACTIVE_PLATFORM", None) is None:
            self.skipTest("no resolved platform to reuse")
        module.get_platform_name()
        self.assertEqual(calls, [])

    def test_the_clocks_hold_a_getter_but_are_not_on_the_timer(self):
        # Holding the getter is not liveness: nothing re-reads these on this
        # tab. It means the Advanced window, which polls its own list, gets
        # the current reading rather than a copy of startup.
        by_name = {row.get("name"): row for row in system_info_rows()}
        for name in ("BCLK", "MCLK", "Uncore", "UCLK", "PSF0 PLL",
                     "DRAM Frequency", "DRAM Ratio", "DDR QCLK Ratio",
                     "Gear Mode"):
            row = by_name.get(name)
            if row is None:
                continue
            with self.subTest(name=name):
                self.assertTrue(callable(row.get("value")))
                self.assertFalse(row.get("live"))

    def test_only_the_sensor_rows_are_read_on_a_timer(self):
        # "live" puts a row on the refresh worker. It belongs to the sensors,
        # which have a window of their own keeping min, max and average; the
        # reading tabs show settings as they were when the tab was drawn.
        tabs = {row.get("Tab") for row in intel_timings.TIMINGS
                if row.get("live")}
        self.assertTrue(tabs <= {intel_timings.SENSOR_TAB},
                        "rows outside the sensors are on the timer: %s" % tabs)

    def test_the_core_frequency_row_is_gone(self):
        # Its only live source was a WMI perf-counter enumeration costing
        # 551 ms of a 567 ms refresh pass, and with nothing on the timer it
        # would have shown a startup sample of a value that never sits still.
        names = {row.get("name") for row in intel_timings.TIMINGS}
        self.assertNotIn("Core Frequency", names)
        self.assertFalse(hasattr(intel_timings, "get_core_frequency"))

    def test_identity_rows_stay_a_reading_rather_than_a_getter(self):
        # These cannot change while the machine runs and each costs a WMI
        # query, so re-reading them every second would buy nothing. The split
        # is the point: this test fails if a later change makes everything a
        # getter on the grounds that getters are tidier.
        by_name = {row.get("name"): row for row in system_info_rows()}
        for name in ("CPU", "Cores / Threads", "Motherboard", "BIOS",
                     "Microcode", "Memory Capacity"):
            row = by_name.get(name)
            if row is None:
                continue
            with self.subTest(name=name):
                self.assertFalse(callable(row.get("value")))
                self.assertFalse(row.get("live"))

    def test_the_module_rows_read_the_module_without_going_on_the_timer(self):
        # These come off the DIMM over SMBus rather than from WMI, so they are
        # getters: resolving them at import would run a bus scan before the
        # platform is even known. Being a getter is not permission to poll --
        # none of them may carry `live`, which is what would put them on the
        # refresh worker.
        by_name = {row.get("name"): row for row in system_info_rows()}
        for name in ("RAM Manufacturer", "DRAM Manufacturer", "DRAM Die",
                     "Part Number", "Serial Number", "Manufactured"):
            row = by_name.get(name)
            self.assertIsNotNone(row, "%s is missing from System Info" % name)
            with self.subTest(name=name):
                self.assertTrue(callable(row.get("value")))
                self.assertFalse(row.get("live"))

    def test_the_platform_identity_rows_are_present_and_sectioned(self):
        # Added to line the tab up with CPU-Z's Mainboard and CPU tabs. Each
        # belongs with what it describes rather than in one identity block.
        placement = {}
        for title, _column, names in intel_timings.SYSTEM_INFO_SECTIONS:
            for name in names:
                placement[name] = title
        for name, section in (("Code Name", "Processor"),
                              ("Technology", "Processor"),
                              ("Chipset", "Motherboard"),
                              ("Southbridge", "Motherboard"),
                              ("LPCIO", "Motherboard"),
                              ("Type", "Memory")):
            with self.subTest(name=name):
                self.assertEqual(placement.get(name), section)

    def test_an_unlisted_cpu_reports_its_model_rather_than_a_code_name(self):
        # The code-name table cannot cover silicon that does not exist yet.
        # Naming an unknown part after its neighbour in the table is the
        # failure mode worth guarding: it would read as a confident answer.
        with cpu_model(0xF9):
            self.assertEqual(intel_timings.get_cpu_codename(),
                             "Family 6 Model 0xF9")
            self.assertIsNone(intel_timings.get_cpu_technology())

    def test_the_bench_cpu_matches_what_cpuz_names_it(self):
        with cpu_model(0xB7):
            self.assertEqual(intel_timings.get_cpu_codename(), "Raptor Lake")
            self.assertEqual(intel_timings.get_cpu_technology(), "10 nm")

    def test_an_unlisted_pch_reports_its_device_id(self):
        # 0x7A04 is the only one measured; anything else prints what it read.
        def config(device, function, offset):
            return 0x7A868086 if offset == 0x00 else 0x11

        intel_timings._clear_identity_caches()
        self.addCleanup(intel_timings._clear_identity_caches)
        with mock.patch.object(intel_timings, "_pci_config_dword",
                               side_effect=config):
            self.assertEqual(intel_timings.get_southbridge_name(),
                             "Intel PCH 0x7A86 rev. 11")

    def test_summary_can_still_reach_the_power_down_reading(self):
        # The System Info row is gone, so Summary's aligned block now places
        # the Misc tab's Power Down row. Summary draws its names from the
        # whole table rather than from one tab, which is what makes that
        # work -- if the Misc row were ever dropped the block would silently
        # come up a line short instead of failing.
        names = {row.get("name") for row in intel_timings.TIMINGS}
        self.assertNotIn("Power Down Mode", names)
        self.assertIn("Power Down", names)

    def test_every_row_is_placed_in_a_section(self):
        # The tab was 26 rows under one General heading. A row nobody placed
        # keeps that heading rather than vanishing, which is how it stays
        # visible long enough to be noticed and placed.
        placed = {name for _title, _column, names
                  in intel_timings.SYSTEM_INFO_SECTIONS for name in names}
        for row in system_info_rows():
            with self.subTest(name=row.get("name")):
                self.assertIn(row.get("name"), placed)
                self.assertNotEqual(row.get("Category"), "General")

    def test_each_section_draws_as_one_block(self):
        # Rows of one category that are not contiguous render as two headings
        # with the same name -- the trap the Timings tab hit.
        seen, previous = [], None
        for row in system_info_rows():
            category = row.get("Category")
            if category != previous:
                with self.subTest(category=category):
                    self.assertNotIn(category, seen)
                seen.append(category)
                previous = category

    def test_every_section_is_in_the_one_column_the_tab_draws(self):
        # System Info is built as a single full-width column and its "Right"
        # frame is an ungridded placeholder, so a section sent there is built
        # and never shown. The board row needs that full width anyway.
        columns = {column for _title, column, _names
                   in intel_timings.SYSTEM_INFO_SECTIONS}
        self.assertEqual(columns, {"Left"})

    def test_no_new_row_leaked_onto_another_tab(self):
        for row in intel_timings.TIMINGS:
            if row.get("Tab") == "System Info":
                continue
            with self.subTest(tab=row.get("Tab"), name=row.get("name")):
                self.assertNotIn(row.get("name"), NEW_ROWS)


class OrderTest(unittest.TestCase):
    """Identity first, then the board, then what is installed in it."""

    def test_platform_and_os_come_before_the_cpu(self):
        names = row_names()
        self.assertLess(names.index("Platform"), names.index("CPU"))
        self.assertLess(names.index("OS"), names.index("CPU"))

    def test_the_rows_follow_the_declared_order_inside_a_section(self):
        # Sectioning groups the rows, so the tab-wide order only has to hold
        # within a section now. Checked per section rather than dropped: the
        # order list is still what decides where a row sits among its own.
        for title, _column, _names in intel_timings.SYSTEM_INFO_SECTIONS:
            names = [row.get("name") for row in system_info_rows()
                     if row.get("Category") == title]
            expected = [name for name in intel_timings.SYSTEM_INFO_ORDER
                        if name in names]
            with self.subTest(section=title):
                self.assertEqual(names, expected)

    def test_the_identity_rows_lead(self):
        # GPU leads its own Graphics section now, beside the card's board,
        # silicon and frame buffer, so it no longer sits between these two.
        self.assertEqual(row_names()[:2], ["OS", "Platform"])

    def test_no_row_is_left_out_of_the_order(self):
        # A row missing from SYSTEM_INFO_ORDER still renders, at the end. This
        # catches it while it is still someone's decision rather than an
        # accident.
        unlisted = [
            name for name in row_names()
            if name not in intel_timings.SYSTEM_INFO_ORDER
        ]
        self.assertEqual(unlisted, [])

    def test_the_module_rows_follow_the_total_capacity(self):
        names = row_names()
        start = names.index("Memory Capacity")
        self.assertEqual(
            names[start + 1:start + 6],
            ["Slots Used", "DIMM Size", "Rank",
             "DRAM Manufacturer", "DRAM Die"],
        )

    def test_the_removed_rows_are_gone(self):
        names = row_names()
        for name in intel_timings.SYSTEM_INFO_REMOVED:
            with self.subTest(name=name):
                self.assertNotIn(name, names)

    def test_the_clock_chain_reads_downward(self):
        # The order asked for: what the memory runs at, the ratios that got it
        # there, then the clocks underneath it. Uncore sits above MCLK/UCLK
        # because it is the ring the controller hangs off, not a memory clock.
        names = row_names()
        clocks = ("DRAM Frequency", "DRAM Ratio", "DDR QCLK Ratio", "BCLK",
                  "Uncore", "MCLK", "UCLK", "PSF0 PLL")
        self.assertEqual([n for n in names if n in clocks], list(clocks))


class ValueTest(unittest.TestCase):
    def test_the_platform_is_named_from_the_detected_profile(self):
        self.assertEqual(value_of("Platform"), "LGA1700 (DDR4)")

    def test_every_supported_intel_profile_has_a_label(self):
        from platform_profiles import LGA1700_DDR4, LGA1700_DDR5, LGA1851

        for profile in (LGA1700_DDR4, LGA1700_DDR5, LGA1851):
            with self.subTest(profile=profile):
                self.assertIn(profile, intel_timings.PLATFORM_LABELS)

    def test_an_unclassified_platform_reports_no_value(self):
        self.assertIsNone(intel_timings.PLATFORM_LABELS.get("am5"))

    def test_the_os_reads_as_cpuz_states_it(self):
        # "Microsoft Windows 11  Professional (x64) Build 22631.6199" on the
        # bench. The revision is patched rather than read, so this asserts the
        # assembly and not whichever cumulative update the machine is on.
        with os_parts(revision=6199, display_version="23H2"):
            self.assertEqual(
                value_of("OS"),
                "Microsoft Windows 11 Professional (x64) 23H2 "
                "Build 22631.6199",
            )

    def test_the_os_still_reads_when_the_revision_is_unavailable(self):
        # The UBR is the one part WMI does not carry, so it is the one part
        # that can go missing. The row must not lose the build with it.
        with os_parts(revision=None, display_version=None):
            self.assertEqual(
                value_of("OS"),
                "Microsoft Windows 11 Professional (x64) Build 22631",
            )

    def test_the_feature_update_is_not_taken_from_release_id(self):
        # ReleaseId looks like the right key and is not: Microsoft froze it at
        # "2009" when the naming changed, so reading it would label a 23H2
        # machine 2009. It is the fallback only, for the era before
        # DisplayVersion existed.
        def value(name):
            return {"DisplayVersion": "24H2", "ReleaseId": "2009"}.get(name)

        with mock.patch.object(intel_timings, "_windows_version_value",
                               side_effect=value):
            self.assertEqual(intel_timings.get_windows_display_version(), "24H2")

        with mock.patch.object(intel_timings, "_windows_version_value",
                               side_effect=lambda name: {
                                   "ReleaseId": "1909"}.get(name)):
            self.assertEqual(intel_timings.get_windows_display_version(), "1909")

    def test_the_fallback_display_driver_is_not_listed_as_the_gpu(self):
        gpu = value_of("GPU")
        self.assertEqual(gpu, "NVIDIA GeForce RTX 4070 Ti")
        self.assertNotIn("Basic Display", gpu)

    def test_module_size_and_rank_come_from_smbios(self):
        self.assertEqual(value_of("DIMM Size"), "16 GB")
        self.assertEqual(value_of("Rank"), "2R")

    def test_the_dram_component_is_split_into_maker_and_die(self):
        self.assertEqual(value_of("DRAM Manufacturer"), "Samsung")
        self.assertEqual(value_of("DRAM Die"), "B-die")


class UnavailableValueTest(unittest.TestCase):
    """An unreadable field must render like every other missing value."""

    def test_an_unknown_field_reports_no_value(self):
        self.assertIsNone(intel_timings._dimm_field("not a field"))

    def test_no_modules_reports_no_value(self):
        import dimm_inventory

        original = dimm_inventory.read_modules
        dimm_inventory.read_modules = lambda *a, **k: []
        self.addCleanup(setattr, dimm_inventory, "read_modules", original)

        for field in ("size", "rank", "dram_manufacturer", "dram_die"):
            with self.subTest(field=field):
                self.assertIsNone(intel_timings._dimm_field(field))

    def test_an_unlisted_kit_reports_no_die_rather_than_a_guess(self):
        import dimm_inventory

        original = dimm_inventory.read_modules
        dimm_inventory.read_modules = lambda *a, **k: [{
            "capacity_gb": 16, "rank_count": 2, "ic": "Unknown IC",
        }]
        self.addCleanup(setattr, dimm_inventory, "read_modules", original)

        self.assertIsNone(intel_timings._dimm_field("dram_manufacturer"))
        self.assertIsNone(intel_timings._dimm_field("dram_die"))
        # The SMBIOS-backed rows still answer.
        self.assertEqual(intel_timings._dimm_field("size"), "16 GB")

    def test_a_mixed_kit_shows_both_values_rather_than_hiding_one(self):
        import dimm_inventory

        original = dimm_inventory.read_modules
        dimm_inventory.read_modules = lambda *a, **k: [
            {"capacity_gb": 16, "rank_count": 2, "ic": "Samsung B-die"},
            {"capacity_gb": 8, "rank_count": 1, "ic": "SK hynix CJR"},
        ]
        self.addCleanup(setattr, dimm_inventory, "read_modules", original)

        self.assertEqual(intel_timings._dimm_field("size"), "16 GB / 8 GB")
        self.assertEqual(intel_timings._dimm_field("rank"), "2R / 1R")
        self.assertEqual(
            intel_timings._dimm_field("dram_manufacturer"),
            "Samsung / SK hynix",
        )


class MegahertzFormatTest(unittest.TestCase):
    """The clock rows are read down one column and must be written alike."""

    def test_a_whole_number_carries_no_decimal_point(self):
        # Uncore printed "5000.0Mhz" beside an MCLK of "2000 Mhz".
        self.assertEqual(intel_timings._mhz(5000.0), "5000 Mhz")

    def test_the_unit_is_separated_from_the_number(self):
        self.assertEqual(intel_timings._mhz(100.0), "100 Mhz")

    def test_a_real_fraction_survives(self):
        # Trimming must lose what rounding added, not the reading itself.
        self.assertEqual(intel_timings._mhz(4987.5), "4987.5 Mhz")
        self.assertEqual(intel_timings._mhz(1066.666), "1066.666 Mhz")

    def test_an_integer_reads_the_same_as_its_float(self):
        self.assertEqual(intel_timings._mhz(2000), intel_timings._mhz(2000.0))


class LpcioNameTest(unittest.TestCase):
    """The row names whichever sensor chip the board actually answered with.

    It used to ask the NCT679x reader by name, so it read nothing on the
    Z790-P: that board's NCT6687D answers 0xD592, which is a Nuvoton the
    NCT679x reader does not know but the NCT668x one does. The row went blank
    on a board whose Sensors tab was reading that very chip.
    """

    def _profile(self, module, chip_name):
        reader = type("Reader", (), {"chip_name": chip_name})
        reader.__module__ = module
        intel_timings.get_lpcio_name.cache_clear()
        self.addCleanup(intel_timings.get_lpcio_name.cache_clear)
        return mock.patch(
            "intel_board_sensors.board_sensor_profile",
            return_value={"reader": reader()},
        )

    def test_it_names_the_chip_the_board_profile_found(self):
        with self._profile("superio_lpc", "NCT6687D"):
            self.assertEqual(intel_timings.get_lpcio_name(), "Nuvoton NCT6687D")

    def test_the_other_nuvoton_transport_is_named_the_same_way(self):
        with self._profile("nct679x", "NCT6798D"):
            self.assertEqual(intel_timings.get_lpcio_name(), "Nuvoton NCT6798D")

    def test_an_ite_board_is_not_called_a_nuvoton(self):
        with self._profile("ite_superio", "IT8696E"):
            self.assertEqual(intel_timings.get_lpcio_name(), "ITE IT8696E")

    def test_a_transport_with_no_known_vendor_still_names_its_chip(self):
        with self._profile("some_new_reader", "XYZ123"):
            self.assertEqual(intel_timings.get_lpcio_name(), "XYZ123")

    def test_no_chip_reports_nothing(self):
        intel_timings.get_lpcio_name.cache_clear()
        self.addCleanup(intel_timings.get_lpcio_name.cache_clear)
        with mock.patch("intel_board_sensors.board_sensor_profile",
                        return_value=None):
            self.assertIsNone(intel_timings.get_lpcio_name())


class PlacementHelperTest(unittest.TestCase):
    def test_rows_are_appended_when_the_neighbour_is_missing(self):
        row = intel_timings._system_info_row("Nowhere", lambda: "x")
        self.addCleanup(intel_timings.TIMINGS.remove, row)

        placed = intel_timings._place_system_info_rows("no such row", [row])

        self.assertFalse(placed)
        self.assertIs(intel_timings.TIMINGS[-1], row)


class ChannelLayoutTest(unittest.TestCase):
    """DDR5 counts sub-channels; DDR4 counts channels.

    The reference tools both count the sub-channels -- MemTweakIt "Channels
    4", ASRock's configurator "Quad" -- against the same two populated DIMM
    slots. This tool drew four RTL groups while calling the memory dual
    channel, which disagreed with them and with itself.
    """

    def test_ddr5_doubles_the_populated_channels(self):
        # Two DIMM channels, four 32-bit sub-channels, one per trained RTL.
        self.assertEqual(
            intel_timings.channel_layout_name(2, "DDR5"), "Quad Channel")
        self.assertEqual(
            intel_timings.channel_layout_name(1, "DDR5"), "Dual Channel")

    def test_ddr4_counts_the_channels_themselves(self):
        # No sub-channels there, so two DIMM channels stay dual.
        self.assertEqual(
            intel_timings.channel_layout_name(2, "DDR4"), "Dual Channel")
        self.assertEqual(
            intel_timings.channel_layout_name(1, "DDR4"), "Single Channel")

    def test_an_unnamed_count_still_reports_a_number(self):
        # Better a bare count than nothing on a layout the table skips.
        self.assertEqual(
            intel_timings.channel_layout_name(5, "DDR4"), "5 Channels")

    def test_nothing_populated_is_no_answer_rather_than_a_wrong_one(self):
        # None, so the caller falls through to the slot-tag reading instead
        # of publishing "0 Channels" as though it had measured something.
        self.assertIsNone(intel_timings.channel_layout_name(0, "DDR5"))


if __name__ == "__main__":
    unittest.main()
