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

"""Experimental desktop Ryzen 7000 (Raphael) support.

No Raphael capture exists in this project. These tests pin the ZenStates-Core
layouts the code was written from and the checks that keep a wrong layout
from reaching the screen; they cannot say the layouts are right.
"""

import contextlib
import struct
import unittest

from rochviewer.amd.apob import (
    GraniteRidgeApobReader,
    RAPHAEL_BLOCK_SIZE,
    decode_raphael_training_block,
    parse_raphael_apob_table,
)
from rochviewer.amd.smu_clocks import (
    GRANITE_RIDGE_CLOCK_TABLES,
    RAPHAEL_CLOCK_TABLES,
    RsmuClockReader,
    clock_tables_for_cpu,
)
from rochviewer.platform_profiles import is_granite_ridge_cpu, is_raphael_cpu

RAPHAEL = "AMD Ryzen 7 7800X3D 8-Core Processor"
GRANITE_RIDGE = "AMD Ryzen 7 9850X3D 8-Core Processor"


class RaphaelIdentityTest(unittest.TestCase):
    def test_desktop_ryzen_7000_is_raphael(self):
        for name in (RAPHAEL, "AMD Ryzen 9 7950X 16-Core Processor",
                     "AMD Ryzen 5 7600 6-Core Processor"):
            with self.subTest(name=name):
                self.assertTrue(is_raphael_cpu(name))
                self.assertFalse(is_granite_ridge_cpu(name))

    def test_other_parts_are_not(self):
        for name in (GRANITE_RIDGE,
                     "AMD Ryzen 7 8700G w/ Radeon 780M Graphics",
                     "AMD Ryzen 9 7940HS w/ Radeon 780M Graphics", ""):
            with self.subTest(name=name):
                self.assertFalse(is_raphael_cpu(name))


def as_dword(value):
    return struct.unpack("<I", struct.pack("<f", value))[0]


class FakeMailbox:
    """Just enough of InpOutRsmuClockAccess to run read_clocks."""

    def __init__(self, version, base, dwords):
        self.version = version
        self.base = base
        self.dwords = dwords
        self._arg0 = 0
        self._arg1 = 0
        self._response = 1
        self.commands = []
        self.selector = None

    def is_driver_open(self):
        return True

    def read_vendor(self):
        return 0x1022

    def capture_selector(self):
        self.selector = 0x1234

    @property
    def selector_captured(self):
        return self.selector is not None

    def restore_selector(self):
        pass

    def read_response(self):
        return self._response

    def clear_response(self):
        self._response = 0

    def write_arguments(self, values):
        pass

    def issue_command(self, command):
        self.commands.append(command)
        if command == 0x05:
            self._arg0 = self.version
        elif command == 0x04:
            self._arg0, self._arg1 = self.base & 0xFFFFFFFF, self.base >> 32
        self._response = 1

    def read_arg0(self):
        return self._arg0

    def read_arg1(self):
        return self._arg1

    def read_phys_dword(self, address):
        return self.dwords.get(address - self.base, 0)


def reader(mailbox, tables, umc_mclk=3000.0):
    return RsmuClockReader(mailbox, mutex=contextlib.nullcontext(),
                           umc_mclk_mhz=umc_mclk, tables=tables)


class RaphaelClockTest(unittest.TestCase):
    def mailbox(self, version=0x540108, fclk=2000.0, uclk=3000.0,
                mclk=3000.0, offsets=(0x118, 0x128, 0x138)):
        return FakeMailbox(version, 0x10000000, {
            offsets[0]: as_dword(fclk), offsets[1]: as_dword(uclk),
            offsets[2]: as_dword(mclk),
        })

    def test_each_cpu_gets_only_its_own_tables(self):
        self.assertIs(clock_tables_for_cpu(RAPHAEL), RAPHAEL_CLOCK_TABLES)
        self.assertIs(clock_tables_for_cpu(GRANITE_RIDGE),
                      GRANITE_RIDGE_CLOCK_TABLES)
        self.assertEqual(
            clock_tables_for_cpu("AMD Ryzen 7 8700G w/ Radeon 780M Graphics"),
            {})
        self.assertFalse(set(RAPHAEL_CLOCK_TABLES)
                         & set(GRANITE_RIDGE_CLOCK_TABLES))

    def test_the_tables_match_zenstates_core(self):
        # PowerTable.cs, 8979d27: the 0x5400xx and 0x5401xx families keep
        # their clocks at 0x118/0x128/0x138; 0x540208 moved them up by 4.
        low = RAPHAEL_CLOCK_TABLES[0x540108]
        self.assertEqual((low.fclk, low.uclk, low.mclk, low.length),
                         (0x118, 0x128, 0x138, 0x6BC))
        high = RAPHAEL_CLOCK_TABLES[0x540208]
        self.assertEqual((high.fclk, high.uclk, high.mclk, high.length),
                         (0x11C, 0x12C, 0x13C, 0x8D0))
        self.assertTrue(all(not layout.verified
                            for layout in RAPHAEL_CLOCK_TABLES.values()))
        self.assertTrue(all(layout.verified
                            for layout in GRANITE_RIDGE_CLOCK_TABLES.values()))

    def test_a_raphael_table_reads_and_says_it_is_unverified(self):
        clocks = reader(self.mailbox(), RAPHAEL_CLOCK_TABLES).read_clocks()
        self.assertIsNotNone(clocks)
        self.assertEqual((clocks.fclk_mhz, clocks.uclk_mhz), (2000.0, 3000.0))
        self.assertFalse(clocks.verified)

    def test_a_ryzen_9000_table_is_refused_on_raphael(self):
        mailbox = self.mailbox(version=0x620105,
                               offsets=(0x11C, 0x12C, 0x13C))
        rsmu = reader(mailbox, RAPHAEL_CLOCK_TABLES)
        self.assertIsNone(rsmu.read_clocks())
        self.assertIn("not an approved version", rsmu.last_error)
        # Refused at the version: the table was never transferred.
        self.assertNotIn(0x03, mailbox.commands)

    def test_an_unverified_table_needs_the_umc_cross_check(self):
        rsmu = reader(self.mailbox(), RAPHAEL_CLOCK_TABLES, umc_mclk=None)
        self.assertIsNone(rsmu.read_clocks())
        self.assertIn("unverified", rsmu.last_error)

    def test_a_wrong_offset_is_caught_by_the_cross_check(self):
        # What a misplaced offset looks like: a clock-shaped number that is
        # not the memory clock the UMC reports.
        rsmu = reader(self.mailbox(uclk=2400.0), RAPHAEL_CLOCK_TABLES)
        self.assertIsNone(rsmu.read_clocks())
        self.assertIn("does not match", rsmu.last_error)


def raphael_block(**overrides):
    block = bytearray(RAPHAEL_BLOCK_SIZE)
    block[0x00] = 1
    block[0x02:0x07] = bytes((0, 0, 6, 5, 7))    # RTT: off, off, 40, 48, 34
    block[0x07] = 1                              # DRAM DQ DS 40
    block[0x08:0x0E] = bytes((0, 0, 1, 5, 5, 5))
    block[0x0E] = 28                             # Proc ODT 40
    block[0x0F] = 30                             # Proc DQ DS 34.3
    block[0x11] = 40                             # Proc CA DS 40
    for offset, value in overrides.items():
        block[int(offset[1:], 16)] = value
    return bytes(block)


def raphael_table(block=None, extended=None):
    """An APOB container in the geometry both parsers walk."""
    block = raphael_block() if block is None else block
    table = bytearray(0x200)
    table[0:4] = b"APOB"
    struct.pack_into("<III", table, 4, 1, len(table), 0x60)
    first = 0x80
    struct.pack_into("<I", table, 0x30, first)
    struct.pack_into("<I", table, first + 0x0C, 0x20)
    main = first + 0x20
    table[main] = 0x01
    table[main + 4] = 0x19
    struct.pack_into("<I", table, main + 0x0C, 0x60)
    table[main + 0x30:main + 0x30 + len(block)] = block
    if extended is not None:
        ext = 0x140
        struct.pack_into("<I", table, 0x34, ext)
        table[ext] = 0x07
        table[ext + 4] = 0x03
        struct.pack_into("<I", table, ext + 0x0C, 0x40)
        table[ext + 0x10:ext + 0x10 + len(extended)] = extended
    return bytes(table)


class RaphaelApobTest(unittest.TestCase):
    def test_the_block_decodes_with_the_zen4_offsets(self):
        values = decode_raphael_training_block(raphael_block())
        self.assertEqual(values["rtt_wr"], "40 RZQ/6")
        self.assertEqual(values["rtt_park"], "48 RZQ/5")
        self.assertEqual(values["rtt_park_dqs"], "34 RZQ/7")
        self.assertEqual(values["rtt_nom_rd"], "Off")
        self.assertEqual(values["dram_dq_ds"], "40 Ω")
        self.assertEqual(values["proc_odt"], "40 Ω")
        self.assertEqual(values["proc_ca_ds"], "40 Ω")
        self.assertEqual(values["ck_odt_a"], "Off")
        # Zen 5's pull-up/pull-down pairs have no Zen 4 counterpart and are
        # left out rather than guessed.
        self.assertNotIn("proc_odt_pu", values)
        self.assertNotIn("dram_dq_ds_pu", values)

    def test_the_table_parse_finds_the_record_after_the_lead(self):
        parsed = parse_raphael_apob_table(raphael_table())
        self.assertEqual(parsed.record_offset, 0xA0 + 0x30)
        self.assertEqual(parsed.values["rtt_wr"], "40 RZQ/6")
        self.assertNotIn("proc_ck_ds", parsed.values)

    def test_the_extended_block_adds_ck_and_cs_drive(self):
        extended = bytearray(raphael_block())
        extended[0x12], extended[0x13] = 60, 30
        parsed = parse_raphael_apob_table(
            raphael_table(extended=bytes(extended)))
        self.assertEqual(parsed.values["proc_ck_ds"], "60 Ω")
        self.assertEqual(parsed.values["proc_cs_ds"], "30 Ω")

    def test_an_implausible_block_is_refused(self):
        for bad in (raphael_block(x04=9),       # RTT code out of range
                    raphael_block(x07=5),       # no such DRAM drive
                    raphael_block(x11=99),      # no such CA drive
                    raphael_block(x02=0, x03=0, x04=0, x05=0, x06=0)):
            with self.subTest(block=bad.hex()):
                with self.assertRaises(ValueError):
                    parse_raphael_apob_table(raphael_table(bad))

    def test_a_ryzen_9000_table_does_not_decode_as_raphael(self):
        from tests.test_amd_apob import _apob_table

        with self.assertRaises(ValueError):
            parse_raphael_apob_table(_apob_table())

    def test_the_reader_uses_the_zen4_layout_when_asked(self):
        table = raphael_table()

        def read_dword(address):
            offset = address - 0x0A200000
            if 0 <= offset < len(table):
                return struct.unpack_from("<I", table, offset)[0]
            return 0

        apob = GraniteRidgeApobReader(read_dword=read_dword,
                                      layout=GraniteRidgeApobReader.ZEN4)
        values = apob.read()
        self.assertIsNotNone(values, apob.last_error)
        self.assertEqual(values["rtt_park"], "48 RZQ/5")
        self.assertEqual(apob.channel_values, {})
        self.assertEqual(len(apob.raw_record), RAPHAEL_BLOCK_SIZE)

    def test_an_unknown_layout_is_refused(self):
        with self.assertRaises(ValueError):
            GraniteRidgeApobReader(layout="zen3")


class RaphaelProfileTest(unittest.TestCase):
    def runtime(self):
        from rochviewer.amd.profile import Am5Runtime
        from tests.test_am5_profile import FakeReader, _oracle_regs

        table = raphael_table()

        def training_reader(layout=GraniteRidgeApobReader.ZEN5):
            def read_dword(address):
                offset = address - 0x0A200000
                if 0 <= offset < len(table):
                    return struct.unpack_from("<I", table, offset)[0]
                return 0
            return GraniteRidgeApobReader(read_dword=read_dword,
                                          layout=layout)

        return Am5Runtime(reader_factory=lambda: FakeReader(_oracle_regs()),
                          training_reader_factory=training_reader,
                          cpu_name_factory=lambda: RAPHAEL)

    def test_training_status_says_unverified(self):
        runtime = self.runtime()
        runtime._load_training()
        self.assertIn("(unverified)", runtime.training_status)
        self.assertEqual(runtime.value("rtt_wr"), "40 RZQ/6")

    def test_the_drive_rows_are_the_zen4_ones_and_single_valued(self):
        from rochviewer.amd.profile import build_timings

        rows = {row["name"]: row for row in build_timings(self.runtime())
                if row.get("Tab") == "Training"}
        self.assertIn("Proc ODT", rows)
        self.assertIn("DRAM DQ DS", rows)
        self.assertNotIn("Proc ODT Pu", rows)
        self.assertNotIn("DRAM DQ DS Pd", rows)
        self.assertNotIn("value_a", rows["RTT WR"])
        self.assertEqual(rows["Proc ODT"]["value"](), "40 Ω")

    def test_the_status_row_carries_the_marker(self):
        from rochviewer.amd.profile import build_timings

        rows = {row["name"]: row for row in build_timings(self.runtime())}
        self.assertIn("(unverified)", rows["Status"]["value"]())

    def test_the_smu_row_reports_the_clock_table_as_unverified(self):
        from rochviewer.amd.profile import build_timings
        from rochviewer.amd.smu_clocks import SmuClocks
        from tests.test_am5_profile import stub_live

        runtime = self.runtime()
        clocks = SmuClocks(version=0x540108, table_base=0x1000,
                           fclk_mhz=2000.0, uclk_mhz=3000.0,
                           mclk_mhz=3000.0, verified=False)
        stub_live(runtime, "clocks", clocks)
        source = runtime._sources["clocks"]
        source.status = source._describe(clocks)
        rows = {row["name"]: row for row in build_timings(runtime)}
        self.assertEqual(rows["SMU Status"]["value"](),
                         "PM-table 0x540108 clocks (unverified)")


if __name__ == "__main__":
    unittest.main()
