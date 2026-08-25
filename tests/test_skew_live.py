# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""Every Skew row must be read from hardware on each refresh.

The compensation registers retrain while the tool is open -- the VssHiFF
fields step between 48 and 49, and CLK's pull-down window overlaps them, so it
moves too. A row whose value was resolved while the table was being built
froze at whatever the state happened to be at startup and then disagreed with
any tool reading the same register live. These tests pin the rows open.
"""

import unittest

from tests.intel_stub import install, restore

intel_timings = None

SKEW_TAB = "Skew"


def setUpModule():
    global intel_timings
    intel_timings = install()


def tearDownModule():
    restore()


def skew_rows():
    return [t for t in intel_timings.TIMINGS if t.get("Tab") == SKEW_TAB]


def frozen_values(row):
    """Concrete readings stored on a row, which is the defect being guarded.

    A row is fine if it carries an address, a dynamic lookup or a getter --
    all of those reach hardware when the tab is drawn. It is broken if it
    carries the reading itself, because that was resolved once while the
    table was built and cannot change afterwards.
    """
    stored = []
    for key in ("value", "value_a", "value_b"):
        reading = row.get(key)
        if reading is None or reading == "" or callable(reading):
            continue
        stored.append((key, reading))
    return stored


class SkewLivenessTest(unittest.TestCase):
    def test_the_tab_has_rows_to_check(self):
        # Guards every other test here from passing vacuously.
        self.assertTrue(skew_rows())

    def test_no_row_holds_a_baked_in_reading(self):
        allowed = intel_timings.SKEW_FIXED_BY_SPECIFICATION
        for row in skew_rows():
            name = row.get("name") or "(blank)"
            if name in allowed:
                continue
            with self.subTest(name=name):
                self.assertEqual(
                    frozen_values(row), [],
                    f"{name} stores a reading taken while building the table")

    def test_the_allowed_constants_are_the_ones_with_nothing_to_read(self):
        # Naming them is the point: the list is short, deliberate and says
        # why. A row added to it to silence a failure would stand out.
        self.assertEqual(intel_timings.SKEW_FIXED_BY_SPECIFICATION,
                         frozenset({"CA VREF", "CS VREF"}))

    def test_the_clk_pulldown_matches_the_reference_tools(self):
        # Bits 16-21, which is what both reference maps specify and what they
        # display. Worth knowing that the window is not an independent field
        # -- 16-19 belong to SComp and 20-21 to VssHiFF, so it moves when
        # VssHiFF does -- but agreeing with the tools this row is compared
        # against is the deliberate choice. Bit 6 would read 19.
        module = intel_timings
        saved = (module.read_physical_memory_int, module.detect_ddr_generation)

        def restore_all():
            (module.read_physical_memory_int,
             module.detect_ddr_generation) = saved

        self.addCleanup(restore_all)
        # 0x031204D9 is the bench's 0x2CE4: bit 6 holds 19, bits 16-21
        # hold 18, and bits 20-25 hold 49. Each window gives its own answer,
        # so the assertion cannot pass by coincidence.
        module.read_physical_memory_int = lambda address, size: 0x031204D9
        module.detect_ddr_generation = lambda: "DDR5"
        self.assertEqual(module._read_ddr4_clk_slew_field("dn"), "18")
        # The three fields it shares with CTL are unchanged.
        self.assertEqual(module._read_ddr4_clk_slew_field("up"), "25")
        self.assertEqual(module._read_ddr4_clk_slew_field("scomp"), "32")
        self.assertEqual(module._read_ddr4_clk_slew_field("vsshiff"), "49")

    def test_the_allowed_constants_say_so_rather_than_showing_a_number(self):
        by_name = {row.get("name"): row for row in skew_rows()}
        for name in intel_timings.SKEW_FIXED_BY_SPECIFICATION:
            row = by_name.get(name)
            if row is None:
                continue
            reading = row.get("value")
            if callable(reading) or reading is None:
                continue
            with self.subTest(name=name):
                self.assertRegex(str(reading), r"fixed|Uses ")

    def test_the_compensation_rows_are_getters(self):
        # The specific block that used to be frozen. Named separately so a
        # regression here reads as what it is rather than as a generic
        # liveness failure.
        groups = set(intel_timings.SLEW_RATE_GROUPS)
        comp = [r for r in skew_rows() if r.get("Category") in groups]
        self.assertTrue(comp)
        for row in comp:
            with self.subTest(name=row.get("name")):
                self.assertTrue(callable(row.get("value")))

    def test_each_compensation_row_reads_its_own_field(self):
        # These getters are built in a loop. Without per-row binding they
        # would all close over the last field and the block would show one
        # number repeated, which still looks like a plausible reading.
        # Checked by recording what each getter asks for rather than by
        # comparing readings: against a stub every register returns the same
        # value, so equal readings would prove nothing either way.
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(
            lambda: setattr(module, "read_physical_memory_int", saved))
        asked = []

        def record(address, size):
            asked.append(address)
            return 0x12345678

        module.read_physical_memory_int = record
        groups = set(module.SLEW_RATE_GROUPS)
        requests = []
        for row in skew_rows():
            if row.get("Category") not in groups:
                continue
            asked.clear()
            requests.append((row["name"], row["value"](), tuple(asked)))

        self.assertTrue(requests)
        for name, reading, addresses in requests:
            with self.subTest(name=name):
                self.assertTrue(addresses, f"{name} read no register")
        # Distinct fields: same register is fine, identical (address, result)
        # for every row is not.
        self.assertGreater(
            len({(reading, addresses) for _, reading, addresses in requests}),
            1)

    def test_a_getter_survives_an_unreadable_register(self):
        # Refreshing must not raise: the tab is redrawn on a timer, and an
        # exception there would take the window with it.
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(
            lambda: setattr(module, "read_physical_memory_int", saved))
        for reading in (None, 0xFFFFFFFF):
            module.read_physical_memory_int = lambda address, size: reading
            groups = set(module.SLEW_RATE_GROUPS)
            for row in skew_rows():
                if row.get("Category") not in groups:
                    continue
                with self.subTest(name=row.get("name"), reading=reading):
                    self.assertEqual(row["value"](), "N/A")


class ReferenceNameTest(unittest.TestCase):
    """The comp-block rows carry the reference tools' own names.

    They were DATA RComp Drv Up, CMD VREFUP and so on. Matching the names
    makes the two tools comparable line by line, which is how three wrong
    readings were found. The registers and bit fields did not change.
    """

    def test_the_group_prefixes_survive_the_reference_spelling(self):
        # Their name is "Data Drv Up" against a heading of DATA. An exact
        # prefix test put every one of those in the SComp group instead.
        self.assertEqual(intel_timings._slew_rate_category("Data Drv Up"),
                         "DATA")
        self.assertEqual(intel_timings._slew_rate_category("CMD Drv Dn"),
                         "CMD")

    def test_each_group_kept_its_rows(self):
        groups = {}
        for row in skew_rows():
            if row.get("Category") in intel_timings.SLEW_RATE_GROUPS:
                groups.setdefault(row["Category"], []).append(row["name"])
        if not groups:
            self.skipTest("platform has no comp block")
        self.assertEqual(len(groups["DATA"]), 5)
        self.assertEqual(len(groups["CMD"]), 4)
        self.assertEqual(len(groups["CLK"]), 4)
        self.assertEqual(len(groups["CTL"]), 5)
        self.assertEqual(len(groups["SComp"]), 3)

    def test_no_row_kept_the_old_spelling(self):
        names = {row.get("name") for row in skew_rows()}
        for stale in ("DATA RComp Drv Up", "CMD RComp Drv Dn",
                      "CLK RComp Drv Up", "CTL RComp Drv Dn",
                      "SComp comp bonus", "DQ VREFUP", "CMD VREFDN"):
            with self.subTest(name=stale):
                self.assertNotIn(stale, names)


class DriveStrengthPairTest(unittest.TestCase):
    """CKE/CS has an up level and no down level.

    A down row was added once -- as CKE CS VREFDN, before these rows took the
    reference tools' names -- on the assumption that every level is a pair
    split across 0x2CEC and 0x2CF0 on matching bits, reading bits 24-31 of the
    down register by symmetry with the up one. It returned a real byte and
    looked like a level, which is what made it convincing. Three sources say
    it is not one: the reference map stops after WrDSClk Dn, neither binary
    contains the string ckecs_vrefdn, and BIOS offers a CKE/CS up setting with
    no down one beside it.
    """

    def test_the_up_level_is_present(self):
        names = {row.get("name") for row in skew_rows()}
        if not any(str(name).startswith("WrDS") for name in names):
            self.skipTest("platform has no drive-strength block")
        self.assertIn("WrDSCke CS Up", names)

    def test_the_down_level_is_not_invented(self):
        names = {row.get("name") for row in intel_timings.TIMINGS}
        self.assertNotIn("WrDSCke CS Dn", names)
        self.assertNotIn("CKE CS VREFDN", names)

    def test_every_other_level_still_pairs(self):
        # The point is that CKE/CS is the exception, not that pairing is
        # wrong: if these stopped pairing the removal above would have taken
        # something real with it.
        names = {row.get("name") for row in skew_rows()}
        if "WrDSCmd Up" not in names:
            self.skipTest("platform has no drive-strength block")
        for up, down in (("WrDS Up", "WrDS Dn"),
                         ("RdODT Up", "RdODT Dn"),
                         ("WrDSCmd Up", "WrDSCmd Dn"),
                         ("WrDSCtl Up", "WrDSCtl Dn"),
                         ("WrDSClk Up", "WrDSClk Dn")):
            with self.subTest(pair=up):
                self.assertIn(up, names)
                self.assertIn(down, names)


class OdtlOffsetTest(unittest.TestCase):
    """Every ODT latency offset carries its sign, the way the tool does.

    Written by hand the six tables drifted: ODTL WR OFF and ODTL WR NT OFF
    printed "2 Clocks" where the other four printed "+2 Clocks" for the same
    kind of value. They are built from one formatter now, so the drift cannot
    come back a table at a time.
    """

    def test_a_positive_offset_is_signed(self):
        self.assertEqual(intel_timings.clock_offset(2), "+2 Clocks")
        self.assertEqual(intel_timings.clock_offset(1), "+1 Clock")

    def test_a_negative_offset_keeps_its_sign_and_its_singular(self):
        self.assertEqual(intel_timings.clock_offset(-2), "-2 Clocks")
        self.assertEqual(intel_timings.clock_offset(-1), "-1 Clock")

    def test_zero_is_not_signed(self):
        # "+0 Clocks" is what "%+d" gives and it reads as a setting rather
        # than as no offset at all.
        self.assertEqual(intel_timings.clock_offset(0), "0 Clocks")

    def test_every_table_signs_its_positives(self):
        tables = ("ODTL_ON_WR", "ODTL_OFF_WR", "ODTL_ON_WR_NT",
                  "ODTL_OFF_WR_NT", "ODTL_ON_RD_NT", "ODTL_OFF_RD_NT")
        for name in tables:
            table = getattr(intel_timings, name)
            with self.subTest(table=name):
                self.assertEqual(sorted(table), list(range(8)))
                for label in table.values():
                    if label == "RFU" or label.startswith("0 "):
                        continue
                    self.assertRegex(label, r"^[+-]\d+ Clocks?$")


if __name__ == "__main__":
    unittest.main()


class Ddr4VrefTest(unittest.TestCase):
    """DDR4 trains one VREFDQ per rank, and it has to reach the row."""

    def test_the_per_device_rows_collapse_to_one(self):
        names = [row.get("name") for row in skew_rows()]
        self.assertIn("DQ VREF", names)
        for device in range(4):
            with self.subTest(device=device):
                self.assertNotIn("DQ VREF D%d" % device, names)

    def test_the_row_reads_the_mr6_decoder(self):
        row = next(r for r in skew_rows() if r.get("name") == "DQ VREF")
        self.assertIs(row.get("value"), intel_timings._decode_ddr4_dq_vref)

    def test_the_row_is_not_left_without_a_source(self):
        # The regression this covers: the DDR5 work split "DQ VREF" into
        # D0-D3, so the DDR4 branch that matched the old name stopped firing.
        # All four rows kept being stripped of their reader and none got one
        # back, leaving the section permanently blank on DDR4.
        row = next(r for r in skew_rows() if r.get("name") == "DQ VREF")
        self.assertTrue(
            row.get("value") is not None
            or (row.get("address") is not None and row.get("parameters"))
            or row.get("dynamic_params")
        )

    def test_the_command_bus_references_come_off(self):
        # DDR5 trains VREFCA and VREFCS in MR11 and MR12. DDR4 defines
        # neither: the command bus references an external half-VDD supply and
        # CS shares it, so there is nothing on the module holding a value.
        # These rows used to print "50.0% (fixed)" and "Uses CA VREF", which
        # state the absence of a reading in the place a reading would go.
        names = [row.get("name") for row in skew_rows()]
        for name in intel_timings.DDR4_ABSENT_VREF_ROWS:
            with self.subTest(name=name):
                self.assertNotIn(name, names)

    def test_the_dq_row_is_what_is_left_of_the_vref_section(self):
        names = [
            row.get("name") for row in skew_rows()
            if row.get("Category") == "VREF"
        ]
        self.assertIn("DQ VREF", names)

    def test_the_range_selector_is_not_read_off_the_ccd_bits(self):
        # Bit 12 was read as the range selector, which put this row at 63.20%
        # instead of 78.20%. It is the top of tCCD_L: bits [12:10] read 4 on
        # the bench and 4 decodes to 8, which is the tCCD_L the BIOS shows. A
        # range bit at 12 would leave tCCD_L at [11:10] = 0, meaning 4.
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(
            setattr, module, "read_physical_memory_int", saved)
        module.read_physical_memory_int = lambda address, size=4: 0x101C
        # 60.0% + 28 * 0.65%, the Range 1 reading.
        self.assertEqual(module._decode_ddr4_dq_vref(), "78.20%")

    def test_the_range_bit_still_selects_the_low_range(self):
        module = intel_timings
        saved = module.read_physical_memory_int
        self.addCleanup(
            setattr, module, "read_physical_memory_int", saved)
        module.read_physical_memory_int = lambda address, size=4: 0x109C
        # Bit 7 set: 45.0% + 28 * 0.65%.
        self.assertEqual(module._decode_ddr4_dq_vref(), "63.20%")


class Ddr4DfeTest(unittest.TestCase):
    """DDR4 DRAM has no decision feedback equaliser to report."""

    def test_no_dfe_rows_are_installed(self):
        dfe = [
            row.get("name") for row in skew_rows()
            if str(row.get("Category", "")).startswith("DFE")
        ]
        self.assertEqual(dfe, [])

    def test_the_gain_does_not_survive_on_its_own(self):
        # It moved off the Jedec tab with the taps. Carrying it across where
        # the taps are not installed would leave one DFE row on a generation
        # that has none.
        names = [row.get("name") for row in intel_timings.TIMINGS]
        for name in ("DFE Gain", "DFE Gain Enable"):
            with self.subTest(name=name):
                self.assertNotIn(name, names)
