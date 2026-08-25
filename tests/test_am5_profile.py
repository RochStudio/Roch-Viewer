import time
import unittest
from unittest import mock

from rochviewer.amd import timings as am5_timings
from rochviewer.amd import smu_clocks as amd_smu_clocks
from rochviewer.amd import smu_power as amd_smu_power
from rochviewer.amd import smu_voltages as amd_smu_voltages
from rochviewer.memory import ddr5_pmic
from rochviewer.sensors import superio_lpc
from rochviewer.amd.smn_mcfg import McfgSmnReader
from tests.test_am5_timings import _oracle_regs

from rochviewer.amd import profile as am5_profile
from rochviewer.amd.profile import (
    BOARD_TEMPERATURES,
    CORE_EFFECTIVE_CLOCK_ROW,
    EM_DASH,
    Am5Runtime,
    build_timings,
)
from rochviewer.sensors.voltage_rails import PER_MODULE_RAILS, RAILS
from rochviewer.amd.smu_voltages import SmuVoltages


class FakeReader:
    def __init__(self, regs):
        self.regs = regs
        self.last_error = ""
        self.calls = []
        self.batch_calls = 0

    def read(self, address):
        self.calls.append(address)
        return self.regs.get(address, 0)

    def read_many(self, addresses):
        self.batch_calls += 1
        return {address: self.regs.get(address, 0) for address in addresses}


class FakeTrainingReader:
    def __init__(self, values, channel_values=None):
        self.values = values
        self.channel_values = channel_values or {}
        self.calls = 0
        self.table_address = 0x0A200000
        self.record_address = 0x0A2000D0
        self.channel_record_addresses = {
            "cha": 0x0A2000D0,
            "chb": 0x0A200100,
        } if channel_values else {}
        self.last_error = ""

    def read(self):
        self.calls += 1
        return dict(self.values)


class _ClocksStub:
    version = 0x620105
    table_base = 0x1000
    fclk_mhz = 2000.0
    uclk_mhz = 2050.0
    mclk_mhz = 4100.0


am5_profile_clocks_stub = _ClocksStub()


def stub_live(runtime, name, value):
    """Seed a live source with a value and mark it freshly read.

    Live rails expire after a fraction of a second so the UI can show movement.
    A test that stubs a cached value must also stamp it, or the expiry sends
    the read to real hardware.
    """
    source = runtime._sources[name]
    source.value = value
    source.stamp = time.monotonic()
    return runtime


def freeze_live(runtime):
    """Mark every live source as just-read, without changing its value."""
    for source in runtime._sources.values():
        if source.stamp is None:
            source.stamp = time.monotonic()
    return runtime


class Am5RuntimeTest(unittest.TestCase):
    def test_default_transport_is_mcfg_ecam(self):
        runtime = Am5Runtime()
        self.assertIs(runtime._reader_factory, McfgSmnReader)

    def test_loads_first_plausible_channel_once(self):
        reader = FakeReader(_oracle_regs())
        created = []

        def factory():
            created.append(True)
            return reader

        runtime = Am5Runtime(reader_factory=factory)
        self.assertEqual(runtime.value("tCL"), 36)
        self.assertEqual(runtime.value("tREFI"), 65535)
        self.assertEqual(len(created), 1)
        self.assertEqual(reader.batch_calls, 1)
        self.assertEqual(runtime.active_base, 0)
        self.assertIn("UMC0", runtime.status)

    def test_failure_is_neutral_and_actionable(self):
        reader = FakeReader({})
        runtime = Am5Runtime(reader_factory=lambda: reader)
        self.assertEqual(runtime.value("tCL"), "—")
        self.assertIn("no plausible", runtime.status.lower())

    def test_profile_has_amd_rows_and_no_intel_terms(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        rows = build_timings(runtime)
        names = {row["name"] for row in rows}
        self.assertTrue({
            "MCLK", "CR", "Gear Down Mode", "BGS",
            "Nitro Rx/Tx/Ctrl", "tCL", "tRFCsb",
        } <= names)
        self.assertFalse({"Gear Mode", "System Agent", "Ring", "CMD Stretch"} & names)
        self.assertIn("AMD SMN/MCFG READ-ONLY",
                      next(row for row in rows
                           if row["name"] == "Read Status")["value"]())

    def test_summary_control_rows_use_requested_labels_and_values(self):
        regs = _oracle_regs()
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(regs))
        by_name = {row["name"]: row for row in build_timings(runtime)}

        self.assertEqual(by_name["Gear Down Mode"]["value"](), "Disabled")
        self.assertEqual(by_name["Nitro Rx/Tx/Ctrl"]["value"](), "1/3/1")

        regs[0x50200] |= 1 << 18
        enabled_runtime = Am5Runtime(reader_factory=lambda: FakeReader(regs))
        enabled = {row["name"]: row for row in build_timings(enabled_runtime)}
        self.assertEqual(enabled["Gear Down Mode"]["value"](), "Enabled")

    def test_rtt_wr_precedes_rtt_nom_wr(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        names = [row["name"] for row in build_timings(runtime)]
        self.assertLess(names.index("RTT WR"), names.index("RTT Nom WR"))

    def test_system_info_rows_follow_the_requested_order(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        names = [
            row["name"] for row in build_timings(runtime)
            if row["Tab"] == "System Info"
        ]
        # Sectioned the way the Intel tab reads: what the machine is, the
        # board, the clock chain, what is installed, how it is set, the card,
        # then what could and could not be read.
        self.assertEqual(names, [
            "OS", "Platform",
            "CPU", "Code Name", "Vendor", "Technology",
            "Cores / Threads", "Microcode",
            # CPU-Z's names for the board, which is why the processor's own
            # vendor row is "Vendor" -- two rows cannot share one name.
            "Manufacturer", "Model", "BIOS", "BIOS Date",
            "Chipset", "Southbridge", "LPCIO", "AGESA",
            "BCLK", "MCLK", "FCLK", "UCLK", "DRAM Frequency", "UCLK:MCLK",
            "DRAM Ratio",
            # The sockets and how they are wired sit above the capacity they
            # add up to, and the part number leads the maker rows because it
            # is what a kit is looked up by.
            "Type", "Slots Used", "Channels", "Memory Capacity",
            "DIMM Size", "Rank",
            "Part Number", "Module Manufacturer",
            "IC Manufacturer", "DRAM Die",
            "Serial Number", "Manufactured",
            "GPU", "Board Manufacturer", "GPU Code Name", "GPU Revision",
            "GPU Technology", "Cores", "ROPs / TMUs",
            "Memory Size", "Memory Type", "Memory Vendor",
            "Bus Width", "Resizable BAR", "Driver Version", "Driver Date",
            "Status", "Read Status", "Training Status",
            "Voltage Status", "Power Status",
        ])

    def test_every_system_info_row_is_placed_in_a_section(self):
        # A row nobody placed would have no heading at all. Failing here is
        # how it stays visible long enough to be noticed and placed.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        placed = {name for _title, names in am5_profile.SYSTEM_INFO_SECTIONS
                  for name in names}
        for row in build_timings(runtime):
            if row["Tab"] != "System Info":
                continue
            with self.subTest(name=row["name"]):
                self.assertIn(row["name"], placed)
                self.assertEqual(row["Category"],
                                 am5_profile.SECTION_OF[row["name"]])

    def test_each_system_info_section_draws_as_one_block(self):
        # Rows of one category that are not contiguous render as two headings
        # with the same name -- the trap the Timings tab hit.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        seen, previous = [], None
        for row in build_timings(runtime):
            if row["Tab"] != "System Info":
                continue
            category = row["Category"]
            if category != previous:
                with self.subTest(category=category):
                    self.assertNotIn(category, seen)
                seen.append(category)
                previous = category
        self.assertEqual(
            seen, [title for title, _names in am5_profile.SYSTEM_INFO_SECTIONS]
        )

    def test_dram_rows_read_spd_and_fall_back_to_the_part_number_table(self):
        # The maker and die must come off the module when the SMBus path is
        # open; the part-number table is a fallback, not the source.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        rows = {row["name"]: row for row in build_timings(runtime)}

        spd = [{"dram_manufacturer": "SK hynix", "dram_die": "A-die"}]
        with mock.patch("rochviewer.memory.ddr5_spd.read_identity", return_value=spd):
            self.assertEqual(rows["IC Manufacturer"]["value"](), "SK hynix")
            self.assertEqual(rows["DRAM Die"]["value"](), "A-die")

        modules = [{"ic": "Micron (die unknown)"}]
        with mock.patch("rochviewer.memory.ddr5_spd.read_identity", return_value=[]), \
                mock.patch("rochviewer.memory.dimm_inventory.read_modules", return_value=modules):
            self.assertEqual(rows["IC Manufacturer"]["value"](), "Micron")
            self.assertEqual(rows["DRAM Die"]["value"](), "—")

    def test_tertiary_groups_cover_every_row_and_split_across_columns(self):
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        by_category = {}
        for row in rows:
            if row["Tab"] != "Timings":
                continue
            by_category.setdefault(row["Category"], []).append(row["name"])

        # The old single 28-row Tertiary block is gone, and every one of its
        # rows still has a home.
        self.assertNotIn("Tertiary", by_category)
        self.assertEqual(by_category["Read to read"],
                         ["tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD"])
        self.assertEqual(by_category["Write to write"],
                         ["tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD"])
        self.assertEqual(by_category["Turnaround"], ["tRDWR", "tWRRD"])
        self.assertEqual(by_category["CAS to CAS"],
                         ["tCCD_L", "tCCD_L_WR", "tCCD_L_WR2"])
        self.assertEqual(by_category["Mode register"],
                         ["tMRD", "tMOD", "tMRDPDA", "tMODPDA"])
        self.assertEqual(by_category["Power down"], ["tCKE", "tXP"])
        self.assertEqual(by_category["Preamble / postamble"],
                         ["tRDPRE", "tRDPOST", "tWRPRE", "tWRPOST"])
        self.assertEqual(by_category["Stagger"], ["tSTAG", "tSTAGsb"])
        self.assertEqual(by_category["PHY"], ["tPHYWRD", "tPHYRDL", "tPHYWRL"])

        # The point of the split is a balanced tab, so the groups have to land
        # on both sides.
        columns = {
            category: {row["Column"] for row in rows
                       if row["Category"] == category}
            for category in by_category
        }
        self.assertEqual(columns["Read to read"], {"Right"})
        self.assertEqual(columns["Write to write"], {"Right"})
        self.assertEqual(columns["Turnaround"], {"Right"})
        self.assertEqual(columns["PHY"], {"Right"})
        self.assertEqual(columns["CAS to CAS"], {"Left"})
        self.assertEqual(columns["Power down"], {"Left"})
        self.assertEqual(columns["Stagger"], {"Left"})
        self.assertEqual(columns["Mode register"], {"Left"})

    def test_every_timing_row_reaches_the_summary(self):
        # The Summary picks timing rows by category. Splitting or renaming a
        # section must not drop rows out of it silently.
        from rochviewer.ui.main import (
            am5_summary_timing_columns,
            AM5_SUMMARY_OMITTED,
            AM5_SUMMARY_PHY_NAMES,
            AM5_SUMMARY_PLACED_NAMES,
            AM5_SUMMARY_SYSTEM_ONLY,
            SIGNAL_CATEGORIES,
        )

        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        expected = {
            row["name"] for row in rows
            if row["Tab"] == "Timings"
            and row["Category"] not in SIGNAL_CATEGORIES
            and row["name"] not in AM5_SUMMARY_PHY_NAMES
            and row["name"] not in AM5_SUMMARY_SYSTEM_ONLY
            # Placed by name under tRC rather than gathered generically.
            and row["name"] not in AM5_SUMMARY_PLACED_NAMES
            # tREFIns restates tREFI in nanoseconds, which earns its place
            # next to the raw interval on the Timings tab and would only
            # repeat the row above it in a Summary column.
            and row["name"] not in AM5_SUMMARY_OMITTED
        }
        first, leftover = am5_summary_timing_columns(rows)
        self.assertEqual(set(first) | set(leftover), expected)

    def test_secondary_section_order(self):
        # Write recovery first, then long/short kept together per pair.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        names = [
            row["name"] for row in build_timings(runtime)
            if row["Category"] == "Secondary"
        ]
        self.assertEqual(names, [
            "tWR", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL",
        ])

    def test_row_names_are_unique_across_every_tab(self):
        # Lookups join on row name, so a duplicate silently hijacks whichever
        # view resolves it second. A temperature row named "CPU" once won the
        # Summary lookup over the CPU-name row and showed 41.4 C as the CPU.
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        names = [row["name"] for row in rows]
        duplicates = sorted({n for n in names if names.count(n) > 1})
        self.assertEqual(duplicates, [], "duplicate row names: %s" % duplicates)

    def test_bgs_alt_reads_its_own_registers_not_bgs(self):
        # BGS Alt is a separate control: it comes from 0x500D0/0x500D4 bits
        # 4..10, while BGS is the pattern comparison on 0x50050/0x50058.
        regs = _oracle_regs()
        regs[am5_timings.REG_BGSA0] = 0x00000000
        regs[am5_timings.REG_BGSA1] = 0x00000000
        by_name = {
            r["name"]: r for r in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(regs))
            )
        }
        self.assertEqual(by_name["BGS Alt"]["value"](), "Disabled")

        regs[am5_timings.REG_BGSA0] = 0x00000010      # bit 4 set
        by_name = {
            r["name"]: r for r in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(regs))
            )
        }
        self.assertEqual(by_name["BGS Alt"]["value"](), "Enabled")

    def test_bgs_alt_is_independent_of_bgs(self):
        # The two must be able to disagree; mirroring BGS would look correct
        # only while they happen to match.
        regs = _oracle_regs()
        regs[am5_timings.REG_BGS0] = 0x87654321        # BGS disabled
        regs[am5_timings.REG_BGS1] = 0x87654321
        regs[am5_timings.REG_BGSA0] = 0x00000010       # BGS Alt enabled
        regs[am5_timings.REG_BGSA1] = 0x00000000
        by_name = {
            r["name"]: r for r in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(regs))
            )
        }
        self.assertEqual(by_name["BGS"]["value"](), "Disabled")
        self.assertEqual(by_name["BGS Alt"]["value"](), "Enabled")

    def test_trfcns_shows_both_intervals_outside_normal_refresh(self):
        # Mixed mode refreshes on tRFC2 and tRFCsb, so one number cannot
        # describe it; tRFC itself is not in effect and is dimmed.
        regs = _oracle_regs()
        regs[am5_timings.REG_PD] = 0x0539114A
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(regs))
        by_name = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(by_name["Refresh Mode"]["value"](), "Mixed")
        # The unit is named once, after both values, rather than on each.
        self.assertEqual(by_name["tRFCns"]["value"](), "117/95 (ns)")
        self.assertTrue(by_name["tRFC"]["dim"]())

    def test_trefins_is_whole_nanoseconds(self):
        # 65535 cycles at 4100 MHz is 15984.15 ns, and the hundredths came
        # from the division rather than from any precision the reading has.
        # Nothing covered the format before, so changing it broke no test --
        # which is the reason for this one. The per-channel columns are the
        # same formatter with a channel argument, so they are covered here
        # too; this fixture has no per-channel records to read them from.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        by_name = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(by_name["tREFIns"]["value"](), "15984 (ns)")

    def test_trfcns_shows_one_interval_in_normal_refresh(self):
        regs = _oracle_regs()
        # Clear the per-bank bit and the fine-grain field: plain Normal refresh.
        regs[am5_timings.REG_PD] = 0x05391148 & ~0x00070002
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(regs))
        by_name = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(by_name["Refresh Mode"]["value"](), "Normal")
        # One interval in effect, so one number -- and the unit still reads
        # the same way it does when there are two.
        self.assertEqual(by_name["tRFCns"]["value"](), "117 (ns)")
        # tRFC is the interval in effect here, so it must not be dimmed.
        self.assertFalse(by_name["tRFC"]["dim"]())

    def test_trfcns_reports_both_channels(self):
        regs = _oracle_regs()
        regs[am5_timings.REG_PD] = 0x0539114A

        class TwoChannelReader(FakeReader):
            """Answer the same registers on both UMC bases."""

            def read_many(self, addresses):
                return {a: self.regs.get(a & 0xFFFFF, 0) for a in addresses}

        runtime = Am5Runtime(reader_factory=lambda: TwoChannelReader(regs))
        row = next(r for r in build_timings(runtime) if r["name"] == "tRFCns")
        self.assertEqual(row["value_a"](), "117/95 (ns)")
        self.assertEqual(row["value_b"](), "117/95 (ns)")

    def test_apob_termination_values_are_lazy_and_exposed_as_rows(self):
        training = FakeTrainingReader({
            "rtt_wr": "RZQ/6 (40 Ω)",
            "ca_odt_a": "480 Ω",
            "proc_dq_ds_pu": "40 Ω",
        })
        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()),
            training_reader_factory=lambda: training,
            cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D",
        )
        rows = build_timings(runtime)
        by_name = {row["name"]: row for row in rows}

        self.assertEqual(training.calls, 0)
        self.assertEqual(by_name["RTT WR"]["value"](), "RZQ/6 (40 Ω)")
        self.assertEqual(by_name["CA ODT A"]["value"](), "480 Ω")
        self.assertEqual(by_name["Proc DQ DS Pu"]["value"](), "40 Ω")
        self.assertEqual(training.calls, 1)
        self.assertIn("0x0A200000", runtime.training_status)

    def test_raphael_never_instantiates_granite_ridge_apob_reader(self):
        calls = []

        def training_factory():
            calls.append("instantiated")
            return FakeTrainingReader({"rtt_wr": "wrong"})

        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()),
            training_reader_factory=training_factory,
            cpu_name_factory=lambda: "AMD Ryzen 7 7800X3D",
        )
        self.assertEqual(runtime.value("rtt_wr"), "—")
        self.assertEqual(calls, [])
        self.assertIn("Granite Ridge", runtime.training_status)

    def test_apob_rows_expose_distinct_cha_chb_values_without_group_name_collision(self):
        channels = {
            "cha": {
                "rtt_wr": "RZQ/6 (40 Ω)",
                "ca_odt_a": "480 Ω",
                "ca_odt_b": "60 Ω",
                "proc_odt_pu": "34.3 Ω",
            },
            "chb": {
                "rtt_wr": "RZQ/4 (60 Ω)",
                "ca_odt_a": "240 Ω",
                "ca_odt_b": "48 Ω",
                "proc_odt_pu": "60 Ω",
            },
        }
        training = FakeTrainingReader(channels["cha"], channel_values=channels)
        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()),
            training_reader_factory=lambda: training,
            cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D",
        )
        by_name = {row["name"]: row for row in build_timings(runtime)}

        self.assertEqual(by_name["RTT WR"]["name_a"], "ChA")
        self.assertEqual(by_name["RTT WR"]["name_b"], "ChB")
        self.assertEqual(by_name["RTT WR"]["value_a"](), "RZQ/6 (40 Ω)")
        self.assertEqual(by_name["RTT WR"]["value_b"](), "RZQ/4 (60 Ω)")
        self.assertEqual(by_name["CA ODT A"]["value_a"](), "480 Ω")
        self.assertEqual(by_name["CA ODT A"]["value_b"](), "240 Ω")
        self.assertEqual(by_name["CA ODT B"]["value_a"](), "60 Ω")
        self.assertEqual(by_name["CA ODT B"]["value_b"](), "48 Ω")
        self.assertEqual(by_name["Proc ODT Pu"]["value_a"](), "34.3 Ω")
        self.assertEqual(by_name["Proc ODT Pu"]["value_b"](), "60 Ω")
        self.assertIn("ChA 0x0A2000D0", runtime.training_status)
        self.assertIn("ChB 0x0A200100", runtime.training_status)

    def test_row_values_are_lazy_callables(self):
        reader = FakeReader(_oracle_regs())
        runtime = Am5Runtime(reader_factory=lambda: reader)
        rows = build_timings(runtime)
        tcl = next(row for row in rows if row["name"] == "tCL")
        self.assertTrue(callable(tcl["value"]))
        self.assertEqual(tcl["value"](), "36")


    def test_phy_rows_expose_distinct_cha_chb_umc_values(self):
        regs = {}
        base_regs = _oracle_regs()
        # Populate both UMC windows with distinct PHY values.
        for base in (0, 0x100000):
            for offset, value in base_regs.items():
                regs[base + offset] = value
        regs[0x50258] = (6 << 24) | (35 << 16) | (21 << 8)   # UMC0 / ChA
        regs[0x100000 + 0x50258] = (7 << 24) | (36 << 16) | (22 << 8)  # UMC1 / ChB

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(regs))
        by_name = {row["name"]: row for row in build_timings(runtime)}

        self.assertEqual(by_name["tPHYRDL"]["name_a"], "ChA")
        self.assertEqual(by_name["tPHYRDL"]["name_b"], "ChB")
        self.assertEqual(by_name["tPHYWRD"]["value_a"](), "6")
        self.assertEqual(by_name["tPHYWRD"]["value_b"](), "7")
        self.assertEqual(by_name["tPHYRDL"]["value_a"](), "35")
        self.assertEqual(by_name["tPHYRDL"]["value_b"](), "36")
        self.assertEqual(by_name["tPHYWRL"]["value_a"](), "21")
        self.assertEqual(by_name["tPHYWRL"]["value_b"](), "22")
        self.assertIn("UMC0", runtime.status)
        self.assertIn("UMC1", runtime.status)

    def test_trdpre_and_twrpre_rows_precede_tmod(self):
        names = [row["name"] for row in build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )]
        self.assertLess(names.index("tRDPRE"), names.index("tMOD"))
        self.assertLess(names.index("tWRPRE"), names.index("tMOD"))


    def test_each_row_is_on_the_page_its_kind_belongs_to(self):
        by_name = {
            row["name"]: row
            for row in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
            )
        }
        # What the controller was configured to do is neither identity nor
        # timing, so it has a page of its own.
        self.assertEqual(by_name["Refresh Mode"]["Tab"], "Misc")
        self.assertEqual(by_name["Gear Down Mode"]["Tab"], "Misc")
        # The command rate reads as a timing, and closes the primary group.
        self.assertEqual(by_name["CR"]["Tab"], "Timings")
        self.assertEqual(by_name["CR"]["Category"], "Primary")
        # What training settled on, rather than what it was told.
        self.assertEqual(by_name["RTT WR"]["Tab"], "Skew")
        self.assertEqual(by_name["Proc ODT Pu"]["Tab"], "Skew")
        self.assertEqual(by_name["MCLK"]["Tab"], "System Info")

    def test_cr_closes_the_primary_group(self):
        names = [
            row["name"] for row in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
            )
            if row.get("Tab") == "Timings" and row.get("Category") == "Primary"
        ]
        self.assertEqual(names, [
            "tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "CR",
        ])

    def test_the_summary_still_gathers_the_signal_rows_by_category(self):
        # They moved tab; the Summary selects them by category, so the panel
        # is unaffected -- which is the thing worth pinning about the move.
        from rochviewer.ui import main

        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        selected = {row["name"] for row in main.summary_signal_timings(rows)}
        for name in ("RTT WR", "CA ODT A", "Proc ODT Pu"):
            with self.subTest(name=name):
                self.assertIn(name, selected)

    def test_the_timing_columns_do_not_pick_up_another_tabs_rows(self):
        from rochviewer.ui import main

        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        first, leftover = main.am5_summary_timing_columns(rows)
        for name in ("RTT WR", "Refresh Mode", "Gear Down Mode"):
            with self.subTest(name=name):
                self.assertNotIn(name, first)
                self.assertNotIn(name, leftover)
        # CR is a Timings row now, so it would fall into the generic columns
        # -- into the middle one, since the priority list does not name it --
        # and appear a second time under tRC where the Summary places it.
        self.assertIn("CR", main.AM5_SUMMARY_PLACED_NAMES)
        self.assertNotIn("CR", first)
        self.assertNotIn("CR", leftover)

    def test_voltage_rows_land_on_their_own_tab_and_columns(self):
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        voltage_rows = [row for row in rows if row.get("rail_key")]
        # Every board-wide rail, which is every rail except the per-module
        # ones each DIMM's own panel reads.
        self.assertEqual(
            len(voltage_rows), len(RAILS) - len(PER_MODULE_RAILS)
        )
        labels = [row["name"] for row in voltage_rows]
        self.assertEqual(labels[0], "VDDCR_VDD")
        self.assertEqual(labels[-1], "VDD_MISC")
        self.assertNotIn("Core VID", labels)
        self.assertIn("VDDCR_SOC", labels)
        self.assertNotIn("DRAM VPP", labels)
        by_name = {row["name"]: row for row in voltage_rows}
        # Every rail sits in the left column; power and temperatures take the
        # right one.
        for row in voltage_rows:
            self.assertEqual(row["Column"], "Left")
            self.assertEqual(row["Tab"], "Sensors")
        # One section for every rail; the domain rides along on the row so
        # anything that wants it can group by it.
        self.assertEqual(by_name["VDDIO"]["Category"], "Voltages")
        self.assertEqual(by_name["VDDIO"]["rail_group"], "Memory")
        self.assertEqual(by_name["VDDCR_VDD"]["rail_group"], "Core")
        self.assertEqual(
            {row["Category"] for row in voltage_rows}, {"Voltages"}
        )

    def test_voltage_rows_read_as_unavailable_when_no_offset_is_confirmed(self):
        # Hermetic: force the empty-map path instead of touching the PM table
        # or the DIMM PMIC, either of which returns live rails on real hardware.
        with mock.patch.object(amd_smu_voltages, "CONFIRMED_VOLTAGE_OFFSETS", {}), \
                mock.patch.object(ddr5_pmic, "CONFIRMED_PMIC_RAILS", {}), \
                mock.patch.object(superio_lpc, "CONFIRMED_SENSORS", {}):
            runtime = Am5Runtime(
                reader_factory=lambda: FakeReader(_oracle_regs()),
                cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D 8-Core Processor",
            )
            rows = build_timings(runtime)
            for row in rows:
                if row.get("rail_key"):
                    self.assertEqual(row["value"](), "—", row["name"])
            status = next(row for row in rows if row["name"] == "Voltage Status")
            self.assertIn("no confirmed PM-table offset", status["value"]())

    def test_unmapped_rails_stay_blank_when_others_are_confirmed(self):
        # A confirmed rail must not make a neighbouring one borrow its reading.
        # All three transports are stubbed so nothing reaches real hardware.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "voltages", SmuVoltages(
            version=0x620105, table_base=0x1000, values={"vddcr_soc": 1.203}
        ))
        stub_live(runtime, "dram", {})
        stub_live(runtime, "board", {})
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["VDDCR_SOC"]["value"](), "1.203 V")
        self.assertEqual(rows["VDDIO"]["value"](), "—")
        # The DRAM rails are per-module and have no set-wide row; the rule
        # still holds at the layer their panels read.
        self.assertEqual(runtime.voltage_value("dram_vdd"), "—")
        self.assertEqual(runtime.voltage_value("dram_vddq"), "—")

    def test_each_rail_reads_from_its_own_transport(self):
        # Three transports feed one view; a value from one must never appear
        # under a rail belonging to another.
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "voltages", SmuVoltages(
            version=0x620105, table_base=0x1000, values={"vddg_iod": 1.082}
        ))
        stub_live(runtime, "dram", {"dram_vdd": 1.530})
        stub_live(runtime, "board", {"vddio_mem": 1.424})
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["VDDG IOD"]["value"](), "1.082 V")   # PM table
        self.assertEqual(rows["VDDIO"]["value"](), "1.424 V")      # Super I/O
        self.assertEqual(runtime.voltage_value("dram_vdd"), 1.530)  # DDR5 PMIC

    def test_dram_rails_come_from_the_pmic_not_the_pm_table(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        # PM table reports nothing; the PMIC supplies the DRAM rails.
        stub_live(runtime, "voltages", None)
        stub_live(runtime, "dram", {
            "dram_vdd": 1.500, "dram_vddq": 1.440, "dram_vpp": 1.800
        })
        stub_live(runtime, "board", {})
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(runtime.voltage_value("dram_vdd"), 1.500)
        self.assertEqual(runtime.voltage_value("dram_vddq"), 1.440)
        self.assertEqual(runtime.voltage_value("dram_vpp"), 1.800)
        self.assertEqual(rows["VDDCR_SOC"]["value"](), "—")

    def test_the_dram_rails_have_no_set_wide_row(self):
        # One board setting drives them, but the modules need not agree: the
        # bench pair sits 15 mV apart on VDDQ, which a single row cannot show.
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        keys = {row.get("rail_key") for row in rows}
        for key in ("dram_vdd", "dram_vddq", "dram_vpp"):
            self.assertNotIn(key, keys)
        self.assertIn("vddio_mem", keys)

    def test_voltage_value_reports_confirmed_rails(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "voltages", SmuVoltages(
            version=0x620105, table_base=0x1000, values={"vddcr_soc": 1.203}
        ))
        self.assertAlmostEqual(runtime.voltage_value("vddcr_soc"), 1.203)
        self.assertEqual(runtime.voltage_value("vddg_iod"), "—")
        row = next(
            row
            for row in build_timings(runtime)
            if row["name"] == "VDDCR_SOC"
        )
        self.assertEqual(row["value"](), "1.203 V")

    def test_a_transient_clock_failure_is_retried(self):
        # Regression: one contended read at startup used to blank FCLK/UCLK for
        # the whole session, because the failure was cached as "attempted".
        calls = []

        def flaky(cpu_name="", umc_mclk_mhz=None):
            calls.append(True)
            if len(calls) == 1:
                return None
            return am5_profile_clocks_stub

        from rochviewer.amd import smu_clocks
        with mock.patch.object(amd_smu_clocks, "read_smu_clocks", flaky):
            runtime = Am5Runtime(
                reader_factory=lambda: FakeReader(_oracle_regs()),
                cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D 8-Core Processor",
            )
            self.assertIsNone(runtime.clocks())
            self.assertIs(runtime.clocks(), am5_profile_clocks_stub)
        self.assertEqual(len(calls), 2)

    def test_repeated_clock_failures_eventually_stop_retrying(self):
        calls = []

        def always_fails(cpu_name="", umc_mclk_mhz=None):
            calls.append(True)
            return None

        from rochviewer.amd import smu_clocks
        with mock.patch.object(amd_smu_clocks, "read_smu_clocks", always_fails):
            runtime = Am5Runtime(
                reader_factory=lambda: FakeReader(_oracle_regs()),
                cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D 8-Core Processor",
            )
            for _ in range(10):
                self.assertIsNone(runtime.clocks())
        self.assertEqual(len(calls), 3)

    def test_a_lost_read_keeps_the_last_good_voltage(self):
        # These reads race other monitoring tools for the PCI mutex. Losing
        # once must not blank a rail that was reading fine a moment ago.
        from rochviewer.amd import smu_voltages as module

        good = SmuVoltages(
            version=0x620105, table_base=0x1000, values={"vddcr_soc": 1.203}
        )
        calls = []

        def flaky(cpu_name=""):
            calls.append(True)
            return good if len(calls) == 1 else None

        with mock.patch.object(module, "read_smu_voltages", flaky):
            runtime = Am5Runtime(
                reader_factory=lambda: FakeReader(_oracle_regs()),
                cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D 8-Core Processor",
            )
            self.assertIs(runtime.voltages(), good)
            runtime._sources["voltages"].stamp = None   # force a re-read
            self.assertIs(runtime.voltages(), good)
            self.assertIn("stale", runtime.voltage_status.lower())
        self.assertEqual(len(calls), 2)

    def test_a_lost_dram_read_keeps_the_last_good_rails(self):
        from rochviewer.memory import ddr5_pmic as module

        calls = []

        def flaky():
            calls.append(True)
            return {"dram_vdd": 1.53} if len(calls) == 1 else {}

        with mock.patch.object(module, "read_dram_rails", flaky):
            runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
            self.assertEqual(runtime.dram_rails(), {"dram_vdd": 1.53})
            runtime._sources["dram"].stamp = None
            self.assertEqual(runtime.dram_rails(), {"dram_vdd": 1.53})
            self.assertIn("stale", runtime.dram_rail_status.lower())

    def test_cpu_name_is_queried_only_once(self):
        # It is a WMI query, and callers repeat every second.
        calls = []

        def factory():
            calls.append(True)
            return "AMD Ryzen 7 9850X3D 8-Core Processor"

        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()),
            cpu_name_factory=factory,
        )
        for _ in range(5):
            runtime.cpu_name()
        self.assertEqual(len(calls), 1)

    def test_temperatures_and_limits_share_one_section(self):
        # Thermal Limit is a temperature against a limit, so a heading between
        # the two halves put one row's parts on either side of it.
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        section = [row for row in rows
                   if row["Category"] == "Thermal & Power"]
        self.assertEqual(
            [row["name"] for row in section],
            # The DIMM sensors moved to their module's own telemetry panel,
            # beside the rails from the same PMIC.
            # CPU Temp carries Tctl against its limit, so there is no
            # separate Thermal Limit row repeating the temperature.
            # What the CPU reports about itself, then what the board measures
            # around it, then what it is all drawing.
            # Scalar is not here: it is a setting rather than a reading, so
            # it sits with the limits on Misc. This window keeps a maximum
            # per row, which a value that never moves has no use for.
            ["CPU Temp", "IOD Average", "IOD Hotspot", "L3 Temp",
             "VDDCR_VDD VRM", "VDDCR_SOC VRM", "VDD_MISC VRM",
             "CPU Temp (board)", "VRM Temp (board)", "System Temp",
             "PPT", "TDC", "EDC"],
        )
        for row in section:
            self.assertEqual(row["Tab"], "Sensors")
            self.assertEqual(row["Column"], "Right")
        for gone in ("Temperatures", "Power Limits"):
            self.assertNotIn(gone, {row["Category"] for row in rows})

    def test_power_rows_are_blank_when_no_offset_is_confirmed(self):
        from rochviewer.amd import smu_power

        with mock.patch.object(amd_smu_power, "CONFIRMED_POWER_OFFSETS", {}):
            runtime = Am5Runtime(
                reader_factory=lambda: FakeReader(_oracle_regs()),
                cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D 8-Core Processor",
            )
            rows = build_timings(runtime)
            for row in rows:
                if row["Category"] == "Power Limits":
                    self.assertEqual(row["value"](), "—", row["name"])
            status = next(row for row in rows if row["name"] == "Power Status")
            self.assertIn("no confirmed PM-table offset", status["value"]())

    def test_power_rows_render_current_over_limit(self):
        from rochviewer.amd.smu_power import SmuPower

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "power", SmuPower(
            version=0x620105,
            table_base=0x1000,
            values={"ppt": 41.0244, "tdc": 9.6954},
            limits={"ppt": 162.0, "tdc": 120.0, "edc": 180.0},
            temperatures={"cpu": 43.7},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["PPT"]["value"](), "41.0 / 162.0 W")
        self.assertEqual(rows["TDC"]["value"](), "9.7 / 120.0 A")
        # Limit known but no live value yet: show the limit alone.
        self.assertEqual(rows["EDC"]["value"](), "180.0 A")
        self.assertEqual(rows["Scalar"]["value"](), "—")

    def test_one_status_line_covers_every_transport(self):
        def unused_training_reader():
            raise AssertionError(
                "training is already attempted here; the reader must not be "
                "built"
            )

        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()),
            training_reader_factory=unused_training_reader,
            # Fixed rather than the host's, as every other test in this file
            # does. What CPU runs the suite is not what this test is about.
            cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D",
        )
        freeze_live(runtime)
        runtime._load()
        # The APOB result is an input here, not something to re-derive: the
        # line is built to carry ChA/ChB so the summary can be checked for
        # dropping them. The Status row reads training_status through
        # _load_training, which without this would run the Granite Ridge gate
        # against the machine running the suite and overwrite the line --
        # passing on an AM5 bench and failing everywhere else.
        runtime._training_attempted = True
        runtime.training_status = (
            "AMD APOB READ-ONLY — table 0x0A200000, ChA 0x1, ChB 0x2"
        )
        rows = {row["name"]: row for row in build_timings(runtime)}
        summary = rows["Status"]["value"]()
        self.assertIn("SMN UMC0", summary)
        # The APOB record addresses stay in the dump; the table address is
        # what the summary carries.
        self.assertIn("APOB table 0x0A200000", summary)
        self.assertNotIn("ChA", summary)
        self.assertIn("power", summary)

    def test_a_failing_transport_puts_its_own_message_in_the_line(self):
        # The summary is short only while there is nothing to report.
        def unused_training_reader():
            raise AssertionError(
                "training is already attempted here; the reader must not be "
                "built"
            )

        runtime = Am5Runtime(
            reader_factory=lambda: FakeReader({}),
            training_reader_factory=unused_training_reader,
            cpu_name_factory=lambda: "AMD Ryzen 7 9850X3D",
        )
        freeze_live(runtime)
        # Same reason as the test above, and it bit harder here: on a Granite
        # Ridge host this passed the gate and built the real APOB reader, so
        # a unit test drove the hardware. The assertion below is narrow
        # enough that it passed either way and never said so.
        runtime._training_attempted = True
        rows = {row["name"]: row for row in build_timings(runtime)}
        summary = rows["Status"]["value"]()
        self.assertIn("no plausible", summary)

    def test_the_pm_table_segment_keeps_the_version_it_was_gated_on(self):
        from rochviewer.amd.profile import _pm_table_segment

        self.assertEqual(
            _pm_table_segment("RSMU PM-table 0x620105 @ 0x8000 — 6 rail(s)"),
            "PM-table 0x620105 (6 rails)",
        )
        # A failure is passed through whole rather than compacted away.
        self.assertEqual(
            _pm_table_segment("PM-table read failed: version 0x1 refused"),
            "PM-table read failed: version 0x1 refused",
        )

    def test_the_detailed_status_rows_stay_for_the_dump(self):
        rows = build_timings(
            Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        )
        diagnostic = {row["name"] for row in rows if row.get("diagnostic")}
        self.assertEqual(diagnostic, {
            "Read Status", "Training Status",
            "Voltage Status", "Power Status",
        })
        # The summary itself is not diagnostic: it is what the tab shows.
        summary = next(row for row in rows if row["name"] == "Status")
        self.assertFalse(summary.get("diagnostic"))

    def test_cpu_temp_reads_tctl_against_its_limit(self):
        from rochviewer.amd.smu_power import SmuPower

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "power", SmuPower(
            version=0x620105, table_base=0x1000, values={}, limits={},
            temperatures={"cpu": 41.1, "cpu_limit": 90.0},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["CPU Temp"]["value"](), "41.1 / 90.0 °C")

    def test_cpu_temp_follows_a_limit_that_was_lowered(self):
        # The limit is a setting, not a constant: a hardcoded 95 would have
        # claimed five degrees that are not there on a board reporting 90.
        from rochviewer.amd.smu_power import SmuPower

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "power", SmuPower(
            version=0x620105, table_base=0x1000, values={}, limits={},
            temperatures={"cpu": 41.1, "cpu_limit": 80.0},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["CPU Temp"]["value"](), "41.1 / 80.0 °C")

    def test_cpu_temp_needs_both_halves(self):
        from rochviewer.amd.smu_power import SmuPower

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "power", SmuPower(
            version=0x620105, table_base=0x1000, values={}, limits={},
            temperatures={"cpu": 41.1},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["CPU Temp"]["value"](), "—")

    def test_scalar_row_shows_what_the_smu_getter_returned(self):
        from rochviewer.amd.smu_power import SmuPower

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "power", SmuPower(
            version=0x620105,
            table_base=0x1000,
            values={"scalar": 1.0},
            limits={},
            temperatures={},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["Scalar"]["value"](), "1.00 x")

        stub_live(runtime, "power", SmuPower(
            version=0x620105, table_base=0x1000,
            values={"scalar": 10.0}, limits={}, temperatures={},
        ))
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["Scalar"]["value"](), "10.00 x")

    def test_trfc_nanoseconds_precedes_trfc_in_profile_order(self):
        names = [
            row["name"]
            for row in build_timings(
                Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
            )
        ]
        self.assertLess(names.index("tRFCns"), names.index("tRFC"))


class ClockSectionTest(unittest.TestCase):
    """The Telemetry window leads with the clocks, the way Intel's does."""

    def _clock_rows(self):
        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        # Filtered by tab as well as category: System Info has a Clocks
        # section too, and a category name only means something within a tab.
        return [row for row in build_timings(runtime)
                if row.get("Category") == "Clocks"
                and row.get("Tab") == "Sensors"]

    def test_the_aggregates_read_from_the_bus_up(self):
        names = [row["name"] for row in self._clock_rows()
                 if not row.get("Parent")]
        self.assertEqual(names, [
            "Bus Clock", "Core Clock (avg)", CORE_EFFECTIVE_CLOCK_ROW,
            # Named as HWiNFO names them, which is also what keeps them from
            # colliding with the System Info rows called FCLK and UCLK.
            "Infinity Fabric Clock (FCLK)", "Memory Controller Clock (UCLK)",
        ])

    def test_the_memory_clocks_keep_their_own_names_here(self):
        # The fabric and controller clocks were asked for in this window, so
        # they carry HWiNFO's longer names. The bare MCLK/UCLK/FCLK rows stay
        # on System Info: a row name is the key every other view joins on,
        # and the same name twice would collide.
        names = {row["name"] for row in self._clock_rows()}
        for name in ("MCLK", "UCLK", "FCLK", "DRAM Frequency"):
            with self.subTest(name=name):
                self.assertNotIn(name, names)
        self.assertIn("Infinity Fabric Clock (FCLK)", names)
        self.assertIn("Memory Controller Clock (UCLK)", names)

    def test_every_clock_row_is_live(self):
        # A clock read once is the one thing this window exists not to show.
        for row in self._clock_rows():
            with self.subTest(row=row["name"]):
                self.assertTrue(row.get("live"))

    def test_the_per_processor_rows_fold_under_the_effective_clock(self):
        children = [row for row in self._clock_rows() if row.get("Parent")]
        self.assertTrue(children)
        for row in children:
            with self.subTest(row=row["name"]):
                self.assertEqual(row["Parent"], CORE_EFFECTIVE_CLOCK_ROW)

    def test_counters_that_do_not_answer_leave_the_parent_standing_alone(self):
        with mock.patch.dict("sys.modules", {"rochviewer.sensors.cpu_clocks": None}):
            rows = am5_profile._per_core_clock_rows()
        self.assertEqual(rows, [])

    def test_a_processor_with_no_reading_yet_shows_a_dash(self):
        # core_clock_text returns None until an interval has been measured,
        # and a row's text reaches the label verbatim.
        fake = mock.Mock(core_labels=lambda: ["CPU 0"],
                         core_clock_text=lambda _index: None)
        with mock.patch.dict("sys.modules", {"rochviewer.sensors.cpu_clocks": fake}):
            rows = am5_profile._per_core_clock_rows()
        self.assertEqual(rows[0]["value"](), EM_DASH)


class BoardTemperatureTest(unittest.TestCase):
    """Only the thermistors this board was confirmed to carry."""

    def _rows(self, temperatures):
        runtime = mock.Mock()
        runtime.board_temperatures.return_value = temperatures
        return {row["name"]: row["value"]()
                for row in am5_profile._board_temperature_rows(runtime)}

    def test_the_confirmed_three_are_shown(self):
        self.assertEqual(
            [label for label, _key in BOARD_TEMPERATURES],
            ["CPU Temp (board)", "VRM Temp (board)", "System Temp"],
        )

    def test_the_unconfirmed_channels_are_not_shown(self):
        # pch never moved through a 34 C swing and socket reads 0.0; a stuck
        # channel and a genuinely steady one are indistinguishable from here.
        keys = {key for _label, key in BOARD_TEMPERATURES}
        self.assertNotIn("pch", keys)
        self.assertNotIn("socket", keys)

    def test_each_row_reads_its_own_channel(self):
        rows = self._rows({"cpu": 75.0, "vrm": 32.0, "system": 31.5})
        self.assertEqual(rows["CPU Temp (board)"], "75.0 °C")
        self.assertEqual(rows["VRM Temp (board)"], "32.0 °C")
        self.assertEqual(rows["System Temp"], "31.5 °C")

    def test_a_channel_that_did_not_answer_reads_as_a_dash(self):
        rows = self._rows({"cpu": 41.0})
        self.assertEqual(rows["CPU Temp (board)"], "41.0 °C")
        self.assertEqual(rows["VRM Temp (board)"], EM_DASH)

    def test_no_sensors_at_all_blanks_every_row(self):
        for value in ({}, None):
            with self.subTest(value=value):
                rows = self._rows(value)
                self.assertEqual(set(rows.values()), {EM_DASH})


class SummaryStaggerOrderTest(unittest.TestCase):
    """Stagger reads after the CAS-to-CAS group in the Summary column."""

    def _middle(self):
        from rochviewer.ui import main

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        rows = build_timings(runtime)
        return main.am5_summary_timing_columns(rows)[1]

    def test_stagger_follows_tccd_l_wr2(self):
        middle = self._middle()
        self.assertEqual(
            middle[middle.index("tCCD_L_WR2") + 1:middle.index("tCCD_L_WR2") + 3],
            ["tSTAG", "tSTAGsb"],
        )

    def test_the_turnarounds_follow_the_same_direction_groups(self):
        # Each same-direction group reads complete before the turnarounds.
        middle = self._middle()
        self.assertLess(middle.index("tRDRDDD"), middle.index("tWRRD"))
        self.assertLess(middle.index("tWRWRDD"), middle.index("tWRRD"))
        self.assertLess(middle.index("tWRRD"), middle.index("tRDWR"))

    def test_the_timings_tab_ends_its_left_column_with_the_mode_registers(self):
        # That tab orders sections from TIMINGS_SECTION_ORDER rather than
        # from profile order, so the two can differ; this is what says what
        # the tab actually draws.
        from rochviewer.ui import main

        runtime = Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()))
        seen = []
        for row in build_timings(runtime):
            if row.get("Tab") != "Timings" or row.get("Column") != "Left":
                continue
            if row["Category"] not in seen:
                seen.append(row["Category"])
        ordered = [name for name, _rows in main.ordered_sections(
            [(name, []) for name in seen], main.TIMINGS_SECTION_ORDER)]
        self.assertEqual(ordered[-1], "Mode register")
        # Stagger still follows CAS to CAS, which is the ordering the
        # Summary's middle column depends on.
        self.assertLess(ordered.index("CAS to CAS"), ordered.index("Stagger"))


class SiliconAndBridgeTest(unittest.TestCase):
    """Names for codes read from the hardware, and nothing for a code we
    have no name for."""

    def _with_cpuid(self, processor_id):
        # The processor fields are read once and kept, because opening a WMI
        # connection costs about a second and four rows were each opening
        # their own. A test that supplies its own CPUID therefore has to
        # clear that cache, or it reads whatever the bench answered earlier
        # and the mock is never consulted.
        cpu = mock.Mock(ProcessorId=processor_id, ExtClock=100, Name="stub")
        connection = mock.Mock()
        connection.Win32_Processor.return_value = [cpu]
        saved = list(am5_profile._PROCESSOR_FACTS)
        am5_profile._PROCESSOR_FACTS.clear()
        try:
            with mock.patch.dict(
                "sys.modules", {"wmi": mock.Mock(WMI=lambda: connection)}
            ):
                return am5_profile._cpu_silicon()
        finally:
            am5_profile._PROCESSOR_FACTS[:] = saved

    def test_this_benchs_cpuid_names_granite_ridge(self):
        # Family 0x1A model 0x44, which is what the bench answers and what
        # the repo's own probe gate independently names.
        self.assertEqual(
            self._with_cpuid("178BFBFF00B40F40"), ("Granite Ridge", "4 nm")
        )

    def test_an_unlisted_part_gets_neither_a_name_nor_a_node(self):
        # Rather than inheriting its neighbour's: the node especially cannot
        # be read from anything, so an unlisted part must not borrow one.
        self.assertEqual(self._with_cpuid("178BFBFF00A20F12"), (None, None))

    def test_an_unreadable_cpuid_is_not_an_error(self):
        with mock.patch.dict("sys.modules", {"wmi": None}):
            self.assertEqual(am5_profile._cpu_silicon(), (None, None))

    def _bridge(self, reader, device, revision):
        with mock.patch("rochviewer.system_identity.pci_device_and_revision",
                        return_value=(device, revision)):
            return reader()

    def test_the_chipset_reads_the_host_bridge(self):
        with mock.patch.object(am5_profile, "_cpu_silicon",
                               return_value=("Granite Ridge", "4 nm")):
            self.assertEqual(
                self._bridge(am5_profile._chipset, 0x14D8, 0x00),
                "AMD Granite Ridge rev. 00",
            )

    def test_an_unknown_host_bridge_reads_nothing(self):
        # An Intel board answers here too; naming its bridge from an AMD
        # table is the mistake this row exists not to make.
        self.assertEqual(
            self._bridge(am5_profile._chipset, 0x7A86, 0x11), EM_DASH
        )

    def test_the_southbridge_is_the_fch_by_its_lpc_bridge(self):
        self.assertEqual(
            self._bridge(am5_profile._southbridge, 0x790E, 0x51),
            "AMD FCH rev. 51",
        )

    def test_an_absent_bridge_reads_nothing(self):
        self.assertEqual(
            self._bridge(am5_profile._southbridge, None, None), EM_DASH
        )


if __name__ == "__main__":
    unittest.main()
