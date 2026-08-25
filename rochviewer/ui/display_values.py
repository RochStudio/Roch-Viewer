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

"""Small UI helpers shared by platform timing profiles."""

def is_dual_timing(timing):
    """Return whether a timing definition has independently displayed sides.

    Lives here rather than in main so a platform profile can ask the question
    while it is still building its own table, without importing the GUI.
    """
    return (
        ("parameters_a" in timing and "parameters_b" in timing)
        or ("dynamic_params_a" in timing and "dynamic_params_b" in timing)
        or "value_a" in timing
        or "value_b" in timing
    )


def resolve_display_value(value, unavailable="—"):
    """Evaluate a lazy value and return safe display text."""
    try:
        resolved = value() if callable(value) else value
        if resolved is None:
            return unavailable
        return str(resolved)
    except Exception:
        return unavailable


# Everything that moves lives in the Sensor Telemetry window instead of a tab,
# where each reading is kept with its minimum, maximum and average. A tab shows
# one instant, which is the wrong shape for a rail that only sags under load.
# The rows stay in TIMINGS: the Summary column and the text dump still read them.
WINDOWED_TABS = frozenset({"Sensors"})


def select_tab_names(timings):
    """Return the standard tabs while omitting empty platform-only pages."""
    populated = {row.get("Tab") for row in timings}
    ordered = [
        "System Info", "Timings", "Skew", "Jedec", "RTL", "Misc",
    ]
    return ["Summary"] + [
        name for name in ordered
        if name in populated and name not in WINDOWED_TABS
    ]
