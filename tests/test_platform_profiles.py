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

import unittest

from rochviewer.platform_profiles import (
    AM5,
    LGA1700_DDR4,
    LGA1700_DDR5,
    LGA1851,
    UNSUPPORTED,
    classify_platform,
    is_granite_ridge_cpu,
)


class PlatformClassificationTest(unittest.TestCase):
    def test_granite_ridge_identity_is_separate_from_raphael(self):
        self.assertTrue(is_granite_ridge_cpu("AMD Ryzen 7 9850X3D"))
        self.assertTrue(is_granite_ridge_cpu("AMD Ryzen 9 9950X"))
        self.assertFalse(is_granite_ridge_cpu("AMD Ryzen 7 7800X3D"))
        self.assertFalse(is_granite_ridge_cpu("AMD Ryzen 7 8700G"))

    def test_b850_granite_ridge_is_am5(self):
        self.assertEqual(
            classify_platform(
                manufacturer="AuthenticAMD",
                cpu_name="AMD Ryzen 7 9850X3D 8-Core Processor",
                memory_types=(34, 34),
                board_product="MSI B850MPOWER (MS-7E83)",
            ),
            AM5,
        )

    def test_amd_ddr4_is_not_am5(self):
        self.assertEqual(
            classify_platform("AuthenticAMD", "Ryzen 9 5950X", (26,), "X570"),
            UNSUPPORTED,
        )

    def test_mobile_ryzen_ddr5_is_not_desktop_am5_profile(self):
        for cpu_name in (
            "AMD Ryzen 7 7840HS with Radeon 780M Graphics",
            "AMD Ryzen 9 7945HX",
            "AMD Ryzen 7 7840U",
        ):
            with self.subTest(cpu_name=cpu_name):
                self.assertEqual(
                    classify_platform("AuthenticAMD", cpu_name, (34,), "Laptop"),
                    UNSUPPORTED,
                )

    def test_known_desktop_am5_suffixes_are_accepted(self):
        for cpu_name in (
            "AMD Ryzen 7 7800X3D",
            "AMD Ryzen 7 9700X",
            "AMD Ryzen 5 7500F",
            "AMD Ryzen 7 7700",
        ):
            with self.subTest(cpu_name=cpu_name):
                self.assertEqual(
                    classify_platform("AuthenticAMD", cpu_name, (34,), "AM5"),
                    AM5,
                )

    def test_phoenix_and_unvalidated_desktop_apus_are_unsupported(self):
        for cpu_name in (
            "AMD Ryzen 7 8700G",
            "AMD Ryzen 5 8500G",
            "AMD Ryzen 7 8700F",
            "AMD Ryzen 5 9600G",
        ):
            with self.subTest(cpu_name=cpu_name):
                self.assertEqual(
                    classify_platform("AuthenticAMD", cpu_name, (34,), "AM5"),
                    UNSUPPORTED,
                )

    def test_lga1700_ddr4_and_ddr5_are_separate(self):
        self.assertEqual(
            classify_platform("GenuineIntel", "Intel Core i9-14900KF", (26,), "Z790"),
            LGA1700_DDR4,
        )
        self.assertEqual(
            classify_platform("GenuineIntel", "Intel Core i9-14900K", (34,), "Z790"),
            LGA1700_DDR5,
        )

    def test_pre_lga1700_and_mobile_intel_are_unsupported(self):
        cases = (
            ("Intel Core i7-10700K", (26,), "Z490"),
            ("Intel Core i9-13900HX", (34,), "Laptop"),
            ("Intel Core Ultra 9 185H", (34,), "Laptop"),
            ("Intel Core Ultra 9 285H", (34,), "Laptop"),
        )
        for cpu_name, memory_types, board in cases:
            with self.subTest(cpu_name=cpu_name):
                self.assertEqual(
                    classify_platform(
                        "GenuineIntel", cpu_name, memory_types, board
                    ),
                    UNSUPPORTED,
                )

    def test_core_ultra_desktop_is_lga1851(self):
        for cpu_name in (
            "Intel Core Ultra 9 285K",
            "Intel(R) Core(TM) Ultra 7 270K Plus",
            "Intel Core Ultra 5 225F",
        ):
            with self.subTest(cpu_name=cpu_name):
                self.assertEqual(
                    classify_platform(
                        "GenuineIntel", cpu_name, (34,), "Z890"
                    ),
                    LGA1851,
                )

    def test_unknown_vendor_is_unsupported(self):
        self.assertEqual(classify_platform("", "", (), ""), UNSUPPORTED)


if __name__ == "__main__":
    unittest.main()
