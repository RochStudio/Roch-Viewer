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

"""Cover the graphics rows, and which of them are readings.

Everything nvidia_gpu shows is asked of the card. ROP/TMU counts and the
chip's SKU have no entry point on this driver, so they are not shown at all;
these tests pin that, because a table that answers for a card reads exactly
like a measurement.
"""

import contextlib
import unittest
from unittest import mock

from rochviewer.gpu import nvidia_gpu

BENCH_CARD = 0x2786          # AD104, the RTX 4070 on the bench
OTHER_CARD = 0x1B80          # Pascal, a GTX 1080
GIGABYTE = 0x1458


def card(device=BENCH_CARD, subsystem=GIGABYTE, revision=0xA1,
         vendor=nvidia_gpu.NVIDIA_VENDOR_ID):
    return {"vendor_id": vendor, "device_id": device, "revision": revision,
            "subsystem_vendor_id": subsystem}


@contextlib.contextmanager
def machine(pci=None, nvml=None, nvapi=False):
    """Read the card as it would be on a described machine.

    NVAPI is off unless asked for: it cannot be faked usefully here, and every
    value it supplies is already covered by the values it does not.
    """
    patches = [
        mock.patch.object(nvidia_gpu, "_CACHE", []),
        mock.patch.object(nvidia_gpu, "_adapter_identity", return_value=pci),
        mock.patch.object(nvidia_gpu, "_nvml_query", return_value=nvml or {}),
    ]
    if not nvapi:
        patches.append(mock.patch.object(nvidia_gpu, "_Nvapi",
                                         side_effect=OSError))
    with contextlib.ExitStack() as stack:
        for patch in patches:
            stack.enter_context(patch)
        yield


def read(**kwargs):
    with machine(**kwargs):
        return nvidia_gpu.read_gpu(refresh=True)


class UnitCountTest(unittest.TestCase):
    def test_the_bench_card_reports_what_cpuz_reports(self):
        found = read(pci=card(), nvml={"architecture": 8})
        self.assertEqual(found["technology"], "4 nm")
        self.assertEqual(found["revision"], "A1")
        self.assertEqual(found["board_manufacturer"], "GIGABYTE Technology")

    def test_nothing_is_claimed_from_the_device_id_alone(self):
        # Unit counts and a SKU have no entry point to read them from, so a
        # card is never answered for from a table keyed on its id: with NVAPI
        # down there is no code name at all.
        for device in (BENCH_CARD, OTHER_CARD):
            with self.subTest(device=hex(device)):
                found = read(pci=card(device=device))
                self.assertNotIn("rops_tmus", found)
                self.assertNotIn("code_name", found)
        self.assertFalse(hasattr(nvidia_gpu, "GPU_DEVICE_TABLE"))

    def test_the_code_name_is_the_one_the_driver_reports(self):
        class Nvapi:
            def text(self, name):
                return "GB203" if name == "short_name" else None

            def unsigned(self, name):
                return None

        with machine(pci=card()), mock.patch.object(
                nvidia_gpu, "_Nvapi", return_value=Nvapi()):
            found = nvidia_gpu.read_gpu(refresh=True)
        self.assertEqual(found["code_name"], "GB203")

    def test_the_memory_reads_as_the_card_carries_it(self):
        # This RTX 5070 Ti: what the driver leaves usable is 15.89 GB of its
        # 16, and GPU-Z reads the codes beside it as "GDDR7 (Hynix)".
        values = {"frame_buffer_kb": 16662528, "ram_type": 16, "ram_maker": 6}

        class Nvapi:
            def text(self, name):
                return None

            def unsigned(self, name):
                return values.get(name)

        with machine(pci=card()), mock.patch.object(
                nvidia_gpu, "_Nvapi", return_value=Nvapi()):
            found = nvidia_gpu.read_gpu(refresh=True)
        self.assertEqual(found["memory_size"], "16 GB")
        self.assertEqual(found["memory_type"], "GDDR7")
        self.assertEqual(found["memory_vendor"], "SK hynix")

    def test_an_unlisted_board_vendor_prints_its_id(self):
        found = read(pci=card(subsystem=0x1234))
        self.assertEqual(found["board_manufacturer"], "0x1234")

    def test_no_card_at_all_is_an_empty_answer_rather_than_an_error(self):
        self.assertEqual(read(pci=None), {})

    def test_another_vendors_card_is_not_reported_as_this_one(self):
        # The board-vendor table is add-in-board makers, who ship both brands,
        # so an AMD card would otherwise pick up a plausible-looking vendor and
        # revision with nothing behind them.
        self.assertEqual(read(pci=card(vendor=0x1002)), {})


class CacheTest(unittest.TestCase):
    def test_a_failed_read_is_cached_too(self):
        # Cached only on success, a driver missing one export would repeat two
        # DLL loads and an NVAPI init for all thirteen rows, every second.
        with mock.patch.object(nvidia_gpu, "_CACHE", []) as cache, \
                mock.patch.object(nvidia_gpu, "_read_gpu",
                                  side_effect=OSError) as failing:
            self.assertEqual(nvidia_gpu.read_gpu(), {})
            self.assertEqual(nvidia_gpu.read_gpu(), {})
            self.assertEqual(failing.call_count, 1)
            self.assertEqual(cache, [{}])

    def test_a_refresh_replaces_the_cache_rather_than_growing_it(self):
        # It appended, so the list grew on every refresh and _CACHE[0] kept
        # handing back the first answer.
        with machine(pci=card()):
            nvidia_gpu.read_gpu(refresh=True)
            nvidia_gpu.read_gpu(refresh=True)
            self.assertEqual(len(nvidia_gpu._CACHE), 1)


class ResizableBarTest(unittest.TestCase):
    """The aperture is the reading; Enabled/Disabled is what it means."""

    def _bar(self, bar1_total, frame_buffer_total):
        return read(nvml={"bar1_total": bar1_total,
                          "frame_buffer_total": frame_buffer_total}
                    ).get("resizable_bar")

    def test_the_legacy_window_reads_as_disabled(self):
        # 256 MB against a 12 GB card: what this bench reports today.
        self.assertEqual(self._bar(268435456, 12878610432), "Disabled")

    def test_an_aperture_spanning_the_frame_buffer_reads_as_enabled(self):
        self.assertEqual(self._bar(12884901888, 12878610432), "Enabled")

    def test_an_aperture_just_under_the_frame_buffer_still_reads_enabled(self):
        # The aperture is a power of two and the frame buffer is not, so an
        # enabled card can report slightly under its own memory size. Exact
        # comparison would call that disabled.
        self.assertEqual(self._bar(12884901888, 13958643712), "Enabled")

    def test_no_aperture_reported_means_no_row(self):
        self.assertIsNone(self._bar(0, 12878610432))


class MeasuredTableTest(unittest.TestCase):
    def test_only_measured_memory_codes_are_named(self):
        # These two enumerations are unpublished, and the values measured here
        # contradicted the ordering they are usually quoted with. Filling in
        # the rest from memory is the mistake this guards.
        self.assertEqual(nvidia_gpu.NVAPI_RAM_TYPES,
                         {15: "GDDR6X", 16: "GDDR7"})
        self.assertEqual(nvidia_gpu.NVAPI_RAM_MAKERS,
                         {6: "SK hynix", 10: "Micron"})

    def test_an_unmeasured_code_prints_itself(self):
        self.assertEqual(
            nvidia_gpu._named(nvidia_gpu.NVAPI_RAM_TYPES, 9, "Type"), "Type 9"
        )
        self.assertIsNone(
            nvidia_gpu._named(nvidia_gpu.NVAPI_RAM_TYPES, None, "Type")
        )


class AdapterIdentityTest(unittest.TestCase):
    """The PNP device id carries what the PCI bus scan used to walk for."""

    def test_the_bench_adapter_decodes_to_its_pci_identity(self):
        found = nvidia_gpu._adapter_identity([
            r"PCI\VEN_10DE&DEV_2786&SUBSYS_40C61458&REV_A1\4&256A0AA8&0&0008"
        ])
        self.assertEqual(found["vendor_id"], 0x10DE)
        self.assertEqual(found["device_id"], 0x2786)
        # SUBSYS is device then vendor: 40C6 is the board, 1458 GIGABYTE.
        self.assertEqual(found["subsystem_vendor_id"], 0x1458)
        self.assertEqual(found["subsystem_device_id"], 0x40C6)
        self.assertEqual(found["revision"], 0xA1)

    def test_an_id_that_is_not_a_pci_device_is_ignored(self):
        self.assertIsNone(nvidia_gpu._adapter_identity(["ROOT\\BASICDISPLAY"]))
        self.assertIsNone(nvidia_gpu._adapter_identity([""]))
        self.assertIsNone(nvidia_gpu._adapter_identity([]))

if __name__ == "__main__":
    unittest.main()
