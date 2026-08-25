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

"""Platform timing-backend dispatcher.

Only the selected platform module is imported.  This is the safety boundary
that prevents Intel MCHBAR installers/readers from running on AMD hardware.
"""

from rochviewer.platform_profiles import (
    LGA1700_DDR4,
    LGA1700_DDR5,
    LGA1851,
    detect_current_platform,
)


def load_timing_backend(profile):
    if profile in (LGA1700_DDR4, LGA1700_DDR5, LGA1851):
        from rochviewer.intel.intel_timings import TIMINGS, apply_formula

        return TIMINGS, apply_formula
    from rochviewer.unsupported_profile import TIMINGS, apply_formula

    return TIMINGS, apply_formula


ACTIVE_PLATFORM = detect_current_platform()
TIMINGS, apply_formula = load_timing_backend(ACTIVE_PLATFORM)
