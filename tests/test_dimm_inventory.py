import unittest

from dimm_inventory import (
    EM_DASH,
    board_slot_count,
    channel_of,
    parse_slot,
    rank_count,
    rank_numeric,
    rank_short,
    read_modules,
    shared_value,
    slots_by_channel,
    slots_used,
    split_ic,
)


class FakeModule:
    def __init__(self, **fields):
        self.Capacity = str(16 * 1024 ** 3)
        self.PartNumber = "F5-6000J2636G16G"
        self.Tag = "Physical Memory 2"
        self.Manufacturer = "G.Skill"
        self.SMBIOSMemoryType = 34
        self.Attributes = 1
        for name, value in fields.items():
            setattr(self, name, value)


class FakeConnection:
    def __init__(self, modules):
        self._modules = modules

    def Win32_PhysicalMemory(self):
        return self._modules


class RankDecodeTest(unittest.TestCase):
    def test_low_nibble_of_attributes_is_the_rank_count(self):
        self.assertEqual(rank_count(1), 1)
        self.assertEqual(rank_count(2), 2)
        # Only the low nibble counts; upper bits carry other SMBIOS flags.
        self.assertEqual(rank_count(0x21), 1)

    def test_missing_or_bad_attributes_read_as_no_rank_information(self):
        self.assertEqual(rank_count(None), 0)
        self.assertEqual(rank_count(""), 0)
        self.assertEqual(rank_count("not a number"), 0)

    def test_the_two_rank_spellings(self):
        self.assertEqual(rank_short(1), "SR")
        self.assertEqual(rank_short(2), "DR")
        self.assertEqual(rank_short(4), "4R")
        self.assertEqual(rank_short(0), "N/A")
        self.assertEqual(rank_numeric(1), "1R")
        self.assertEqual(rank_numeric(2), "2R")
        self.assertEqual(rank_numeric(0), EM_DASH)


class SplitIcTest(unittest.TestCase):
    def test_maker_and_die_split_off_the_combined_label(self):
        self.assertEqual(split_ic("SK hynix A-die"), ("SK hynix", "A-die"))
        self.assertEqual(split_ic("Samsung B-die"), ("Samsung", "B-die"))
        self.assertEqual(split_ic("SK hynix CJR"), ("SK hynix", "CJR"))

    def test_die_unknown_keeps_the_maker_and_drops_the_parenthetical(self):
        self.assertEqual(split_ic("Micron (die unknown)"), ("Micron", EM_DASH))

    def test_unidentified_ic_reports_neither(self):
        self.assertEqual(split_ic("Unknown IC"), (EM_DASH, EM_DASH))
        self.assertEqual(split_ic(""), (EM_DASH, EM_DASH))
        self.assertEqual(split_ic(None), (EM_DASH, EM_DASH))


class ReadModulesTest(unittest.TestCase):
    def test_one_dict_per_installed_module(self):
        modules = read_modules(FakeConnection([FakeModule(), FakeModule()]))
        self.assertEqual(len(modules), 2)
        module = modules[0]
        self.assertEqual(module["capacity_gb"], 16)
        self.assertEqual(module["capacity"], "16GB")
        self.assertEqual(module["rank_count"], 1)
        self.assertEqual(module["rank"], "SR")
        self.assertEqual(module["ic"], "SK hynix A-die")
        self.assertEqual(module["part_number"], "F5-6000J2636G16G")

    def test_missing_fields_do_not_raise(self):
        bare = FakeModule(Capacity=None, PartNumber=None, Tag=None,
                          Manufacturer=None, Attributes=None)
        modules = read_modules(FakeConnection([bare]))
        self.assertEqual(modules[0]["capacity_gb"], 0)
        self.assertEqual(modules[0]["part_number"], "Unknown")
        self.assertEqual(modules[0]["rank"], "N/A")

    def test_a_failing_query_reports_no_modules(self):
        class Broken:
            def Win32_PhysicalMemory(self):
                raise OSError("WMI unavailable")

        self.assertEqual(read_modules(Broken()), [])

    def test_an_explicit_connection_is_never_cached(self):
        first = read_modules(FakeConnection([FakeModule()]))
        second = read_modules(FakeConnection([FakeModule(), FakeModule()]))
        self.assertEqual(len(first), 1)
        self.assertEqual(len(second), 2)


class SharedValueTest(unittest.TestCase):
    def test_a_matched_kit_reports_the_single_value(self):
        modules = read_modules(FakeConnection([FakeModule(), FakeModule()]))
        self.assertEqual(shared_value(modules, lambda m: m["rank"]), "SR")

    def test_a_mixed_kit_reports_every_distinct_value(self):
        modules = read_modules(FakeConnection([
            FakeModule(), FakeModule(Attributes=2),
        ]))
        self.assertEqual(shared_value(modules, lambda m: m["rank"]), "SR / DR")

    def test_nothing_known_reports_the_em_dash(self):
        self.assertEqual(shared_value([], lambda m: m["rank"]), EM_DASH)
        modules = read_modules(FakeConnection([FakeModule()]))
        self.assertEqual(shared_value(modules, lambda m: EM_DASH), EM_DASH)


class SlotsUsedTest(unittest.TestCase):
    """Populated sockets against the board's real total."""

    class FakeBoard(FakeConnection):
        def __init__(self, modules, devices, product="MS-9999"):
            super().__init__(modules)
            self._devices = devices
            self._product = product

        def Win32_PhysicalMemoryArray(self):
            devices = self._devices

            class Array:
                MemoryDevices = devices
            return [Array()]

        def Win32_BaseBoard(self):
            product = self._product

            class Board:
                Product = product
            return [Board()]

    def test_the_firmware_count_is_used_when_the_board_is_not_listed(self):
        connection = self.FakeBoard([FakeModule(), FakeModule()], 4)
        self.assertEqual(slots_used(connection), "2 of 4")

    def test_a_board_known_to_overstate_its_sockets_is_corrected(self):
        # MS-7E83 declares four devices on a two-slot board.
        connection = self.FakeBoard(
            [FakeModule(), FakeModule()], 4, "B850MPOWER (MS-7E83)"
        )
        self.assertEqual(slots_used(connection), "2 of 2")
        self.assertEqual(board_slot_count(connection), 2)

    def test_the_board_code_is_matched_inside_the_product_string(self):
        for product in ("B850MPOWER (MS-7E83)", "MS-7E83", "ms-7e83 rev 1.0"):
            with self.subTest(product=product):
                connection = self.FakeBoard([FakeModule()], 4, product)
                self.assertEqual(slots_used(connection), "1 of 2")

    def test_no_reported_socket_count_reports_nothing(self):
        # Better silent than implying the board has as many slots as modules.
        self.assertIsNone(slots_used(self.FakeBoard([FakeModule()], 0)))

    def test_a_total_that_cannot_hold_the_modules_is_rejected(self):
        connection = self.FakeBoard([FakeModule()] * 3, 2)
        self.assertIsNone(slots_used(connection))

    def test_a_failing_query_does_not_raise(self):
        class Broken:
            def Win32_PhysicalMemory(self):
                raise OSError("WMI unavailable")

            def Win32_PhysicalMemoryArray(self):
                raise OSError("WMI unavailable")

            def Win32_BaseBoard(self):
                raise OSError("WMI unavailable")

        self.assertIsNone(slots_used(Broken()))


class SlotNameTest(unittest.TestCase):
    """Slots come from the board's own DeviceLocator, never from record order."""

    def test_the_common_desktop_form(self):
        self.assertEqual(parse_slot("Controller0-DIMMA2"), "A2")
        self.assertEqual(parse_slot("Controller1-DIMMB2"), "B2")

    def test_a_bare_locator(self):
        self.assertEqual(parse_slot("DIMM_A1"), "A1")
        self.assertEqual(parse_slot("DIMMB2"), "B2")

    def test_the_channel_and_index_form_numbers_from_one(self):
        self.assertEqual(parse_slot("Controller0-ChannelA-DIMM0"), "A1")
        self.assertEqual(parse_slot("Controller1-ChannelB-DIMM1"), "B2")

    def test_the_controller_and_index_form_numbers_from_one(self):
        # ASUS ROG MAXIMUS Z790 APEX writes this: no channel letter anywhere,
        # so the controller index supplies it. Left unparsed the bottom DIMM
        # strip drew nothing and the channel columns stayed ChA/ChB.
        self.assertEqual(parse_slot("Controller0-DIMM0"), "A1")
        self.assertEqual(parse_slot("Controller1-DIMM0"), "B1")
        self.assertEqual(parse_slot("Controller0-DIMM1"), "A2")

    def test_a_named_slot_wins_over_the_controller_index(self):
        # Both shapes carry "Controller<n>"; the board's own letter is the
        # better answer wherever firmware bothered to write one.
        self.assertEqual(parse_slot("Controller1-DIMMA2"), "A2")
        self.assertEqual(parse_slot("Controller0-ChannelB-DIMM1"), "B2")

    def test_a_lowercase_locator_still_reads(self):
        self.assertEqual(parse_slot("controller0-dimma2"), "A2")
        self.assertEqual(parse_slot("controller1-dimm0"), "B1")

    def test_an_unreadable_locator_is_not_guessed(self):
        for value in ("", None, "Unknown", "SODIMM"):
            with self.subTest(value=value):
                self.assertIsNone(parse_slot(value))

    def test_channel_of_reads_the_leading_letter(self):
        self.assertEqual(channel_of("A2"), "A")
        self.assertEqual(channel_of("B1"), "B")
        self.assertIsNone(channel_of(None))

    def test_slots_group_and_sort_by_channel(self):
        modules = [
            {"slot": "B2"}, {"slot": "A2"}, {"slot": "A1"}, {"slot": None},
        ]
        self.assertEqual(
            slots_by_channel(modules), {"A": ["A1", "A2"], "B": ["B2"]}
        )

    def test_the_records_that_used_to_be_mislabelled(self):
        # The MSI Z790-P returns its two modules as records 1 and 3. A table
        # keyed on that position called them B1 and A1: wrong numbers, and the
        # channels swapped. The locator gets both right.
        modules = [
            {"slot": parse_slot("Controller0-DIMMA2")},
            {"slot": parse_slot("Controller1-DIMMB2")},
        ]
        self.assertEqual(slots_by_channel(modules), {"A": ["A2"], "B": ["B2"]})


class SlotDecodeTest(unittest.TestCase):
    def test_decode_carries_the_slot_and_channel(self):
        modules = read_modules(
            FakeConnection([FakeModule(DeviceLocator="Controller1-DIMMB2")])
        )
        self.assertEqual(modules[0]["slot"], "B2")
        self.assertEqual(modules[0]["channel"], "B")
        self.assertEqual(modules[0]["device_locator"], "Controller1-DIMMB2")

    def test_a_module_without_a_locator_reports_no_slot(self):
        modules = read_modules(FakeConnection([FakeModule(DeviceLocator="")]))
        self.assertIsNone(modules[0]["slot"])
        self.assertIsNone(modules[0]["channel"])


class SerialNumberTest(unittest.TestCase):
    """SMBIOS carries the serial firmware read off the module at POST.

    It is the only route to it on DDR4, where the SPD path that reads the
    serial directly is a DDR5 hub protocol and refuses to run.
    """

    def _serial(self, value):
        modules = read_modules(
            FakeConnection([FakeModule(SerialNumber=value)])
        )
        return modules[0]["serial_number"]

    def test_a_real_serial_is_carried_through(self):
        self.assertEqual(self._serial("0000ABCD"), "0000ABCD")

    def test_an_all_zero_placeholder_is_not_a_serial(self):
        # What the Z790-P bench reports for both sticks. Printing it would put
        # a serial of 00000000 on a module that never gave one.
        self.assertEqual(self._serial("00000000"), "")

    def test_an_all_f_placeholder_is_not_a_serial_either(self):
        self.assertEqual(self._serial("FFFFFFFF"), "")

    def test_a_serial_that_merely_ends_in_zero_survives(self):
        # The placeholder test strips zeroes to decide; it must not strip
        # them from the answer.
        self.assertEqual(self._serial("12345670"), "12345670")

    def test_a_missing_serial_is_empty_rather_than_an_error(self):
        modules = read_modules(FakeConnection([FakeModule(SerialNumber=None)]))
        self.assertEqual(modules[0]["serial_number"], "")


if __name__ == "__main__":
    unittest.main()
