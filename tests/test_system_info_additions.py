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

"""System Info's L3 cache, board revision, PCIe link and VBIOS rows."""

import unittest
from unittest import mock

from rochviewer import system_identity
from rochviewer.amd import profile as am5_profile
from rochviewer.gpu import radeon

EM_DASH = "—"


class CacheAndBoardTextTest(unittest.TestCase):
    def test_the_l3_reads_in_megabytes(self):
        # Win32_Processor.L3CacheSize on the 9850X3D bench.
        self.assertEqual(am5_profile._cache_text(98304), "96 MB")
        self.assertEqual(am5_profile._cache_text(512), "512 KB")
        for missing in (None, 0, "", "n/a"):
            with self.subTest(value=missing):
                self.assertEqual(am5_profile._cache_text(missing), EM_DASH)

    def test_a_placeholder_board_revision_reads_as_missing(self):
        # The X870 bench's Gigabyte board reports "x.x".
        for placeholder in ("x.x", "Default string", "To be filled by O.E.M.",
                            "", None):
            with self.subTest(value=placeholder):
                self.assertEqual(
                    am5_profile._board_revision_text(placeholder), EM_DASH)
        self.assertEqual(am5_profile._board_revision_text(" 1.2 "), "1.2")


class ClockFormatTest(unittest.TestCase):
    def test_the_dram_ratio_drops_zero_decimals(self):
        with mock.patch.object(am5_profile, "_processor_facts",
                               return_value={"ext_clock": 100}):
            runtime = mock.Mock()
            runtime.value.return_value = 3200
            self.assertEqual(am5_profile._dram_ratio(runtime), "64")
            runtime.value.return_value = 3000
            self.assertEqual(am5_profile._dram_ratio(runtime), "60")
        with mock.patch.object(am5_profile, "_processor_facts",
                               return_value={"ext_clock": 100.5}):
            runtime.value.return_value = 3000
            self.assertEqual(am5_profile._dram_ratio(runtime), "59.7")

    def test_fclk_and_uclk_are_whole_mhz(self):
        from rochviewer.amd.smu_clocks import SmuClocks
        from tests.test_am5_profile import FakeReader, _oracle_regs, stub_live

        runtime = am5_profile.Am5Runtime(
            reader_factory=lambda: FakeReader(_oracle_regs()))
        stub_live(runtime, "clocks", SmuClocks(
            version=0x620105, table_base=0x1000, fclk_mhz=2133.0,
            uclk_mhz=3200.0, mclk_mhz=3200.0))
        self.assertEqual(runtime.value("fclk_mhz"), 2133)
        self.assertEqual(runtime.value("uclk_mhz"), 3200)


class PcieLinkTextTest(unittest.TestCase):
    def test_a_link_at_its_best_is_one_figure(self):
        self.assertEqual(radeon.pcie_link_text(
            {"max_gen": 5, "max_width": 16, "gen": 5, "width": 16}),
            "PCIe 5.0 x16")

    def test_a_resting_link_shows_how_it_runs_now(self):
        # The bench card at idle: a Gen 5 x16 slot running Gen 5 x2.
        self.assertEqual(radeon.pcie_link_text(
            {"max_gen": 5, "max_width": 16, "gen": 5, "width": 2}),
            "PCIe 5.0 x16 @ 5.0 x2")
        self.assertEqual(radeon.pcie_link_text(
            {"max_gen": 4, "max_width": 16, "gen": 1, "width": 16}),
            "PCIe 4.0 x16 @ 1.1 x16")

    def test_nothing_read_is_nothing_shown(self):
        self.assertIsNone(radeon.pcie_link_text(None))
        self.assertIsNone(radeon.pcie_link_text(
            {"max_gen": 9, "max_width": 16, "gen": 5, "width": 16}))


def config_space(functions):
    """A fake pci_config_dword over ``{(bus, dev, fn): {offset: dword}}``."""
    def read(device, function, offset, bus=0):
        return functions.get((bus, device, function), {}).get(offset)
    return read


def express_function(port_type, link_cap, link_status):
    """A function whose only capability is PCI Express at 0x40."""
    return {
        0x34: 0x40,
        0x40: PCI_EXPRESS | (port_type << 20),
        0x4C: link_cap,
        0x50: link_status << 16,
    }


PCI_EXPRESS = system_identity.PCI_EXPRESS_CAPABILITY


def speed_width(gen, width):
    return gen | (width << 4)


class PcieLinkReadTest(unittest.TestCase):
    ROOT, UPSTREAM, GPU = (0, 1, 1), (1, 0, 0), (3, 0, 0)

    def read(self, functions, path=None):
        with mock.patch.object(system_identity, "pci_config_dword",
                               config_space(functions)):
            return system_identity.pcie_link(
                path or [self.ROOT, self.UPSTREAM, self.GPU])

    def test_the_link_is_read_at_the_root_port(self):
        # As on the bench: the root port and the card's upstream switch port
        # agree on Gen 5 x16 capable, running x2; the GPU behind the switch
        # would say x16, which is the switch's internal link.
        link = self.read({
            self.ROOT: express_function(4, speed_width(5, 16),
                                        speed_width(5, 2)),
            self.UPSTREAM: express_function(5, speed_width(5, 16),
                                            speed_width(5, 2)),
            self.GPU: express_function(0, speed_width(5, 16),
                                       speed_width(5, 16)),
        })
        self.assertEqual(link, {"max_gen": 5, "max_width": 16,
                                "gen": 5, "width": 2})

    def test_the_best_is_the_lower_of_the_two_ends(self):
        # A Gen 4 card in a Gen 5 slot can train to Gen 4 at most.
        link = self.read({
            self.ROOT: express_function(4, speed_width(5, 16),
                                        speed_width(4, 16)),
            self.UPSTREAM: express_function(5, speed_width(4, 16),
                                            speed_width(4, 16)),
        })
        self.assertEqual((link["max_gen"], link["max_width"]), (4, 16))

    def test_a_path_that_does_not_start_at_a_root_port_is_refused(self):
        self.assertIsNone(self.read({
            self.ROOT: express_function(6, speed_width(5, 16),
                                        speed_width(5, 16)),
            self.UPSTREAM: express_function(5, speed_width(5, 16),
                                            speed_width(5, 16)),
        }))

    def test_missing_config_space_is_nothing_rather_than_zero(self):
        self.assertIsNone(self.read({}))
        self.assertIsNone(self.read({}, path=[self.GPU]))


class RadeonRegistryTextTest(unittest.TestCase):
    def test_the_bios_string_decodes_from_utf16(self):
        raw = "113-EXT114484-100\x00".encode("utf-16-le")
        self.assertEqual(radeon._registry_text(raw), "113-EXT114484-100")
        self.assertEqual(radeon._registry_text(None), "")


if __name__ == "__main__":
    unittest.main()
