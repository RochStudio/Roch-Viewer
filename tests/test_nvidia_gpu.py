"""Cover the graphics rows, and which of them are readings.

Most of nvidia_gpu asks the card. Two rows cannot: ROP/TMU counts have no
entry point on this driver, so they come from a table. These tests pin the
line between the two, because a table that quietly answers for a card it does
not know would read exactly like a measurement.
"""

import contextlib
import unittest
from unittest import mock

import nvidia_gpu

BENCH_CARD = 0x2786          # AD104, the RTX 4070 on the bench
OTHER_CARD = 0x2C05          # GB203, an RTX 5070 Ti: not in the table
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
        self.assertEqual(found["rops_tmus"], "64 / 184")
        self.assertEqual(found["code_name"], "AD104-250")
        self.assertEqual(found["technology"], "4 nm")
        self.assertEqual(found["revision"], "A1")
        self.assertEqual(found["board_manufacturer"], "GIGABYTE Technology")

    def test_an_unlisted_card_gets_no_unit_counts_rather_than_a_neighbours(self):
        # The failure worth guarding: a table answering confidently for a part
        # it has never seen. The rows are absent instead.
        found = read(pci=card(device=OTHER_CARD))
        self.assertNotIn("rops_tmus", found)
        self.assertNotIn("technology", found)
        # No SKU either -- with NVAPI down there is nothing to fall back to.
        self.assertNotIn("code_name", found)

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
        self.assertEqual(nvidia_gpu.NVAPI_RAM_TYPES, {15: "GDDR6X"})
        self.assertEqual(nvidia_gpu.NVAPI_RAM_MAKERS, {10: "Micron"})

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


class DeviceTableTest(unittest.TestCase):
    """The table is a claim, so it has to stay internally checkable.

    TM units are four per SM and an SM is 128 shaders, on every Ada part. The
    shader counts below are what each card reports for itself, so an entry
    whose TM count does not follow from its shader count is a typo, not a SKU
    difference. ROP counts follow from nothing readable and are the vendor's.
    """

    ADA_SHADERS = {
        0x2684: 16384,        # RTX 4090
        0x2704: 9728,         # RTX 4080
        0x2782: 7680,         # RTX 4070 Ti -- read off this bench
        0x2786: 5888,         # RTX 4070
    }
    SHADERS_PER_SM = 128
    TMUS_PER_SM = 4

    def test_the_bench_card_is_listed(self):
        self.assertEqual(
            nvidia_gpu.GPU_DEVICE_TABLE[0x2782], ("AD104-400", 80, 240)
        )

    def test_every_entry_has_a_shader_count_to_check_against(self):
        self.assertEqual(
            set(nvidia_gpu.GPU_DEVICE_TABLE), set(self.ADA_SHADERS)
        )

    def test_tm_units_follow_the_shader_count(self):
        for device, shaders in self.ADA_SHADERS.items():
            with self.subTest(device=hex(device)):
                _, _, tmus = nvidia_gpu.GPU_DEVICE_TABLE[device]
                self.assertEqual(
                    tmus, shaders // self.SHADERS_PER_SM * self.TMUS_PER_SM
                )

    def test_no_two_cards_share_a_row(self):
        # A copied entry is how a table like this starts answering for the
        # wrong card while still looking populated.
        rows = list(nvidia_gpu.GPU_DEVICE_TABLE.values())
        self.assertEqual(len(rows), len(set(rows)))


if __name__ == "__main__":
    unittest.main()
