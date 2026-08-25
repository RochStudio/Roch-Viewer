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

"""Safe neutral profile used when platform detection is inconclusive."""

EM_DASH = "—"

TIMINGS = [
    {
        "name": "Platform",
        "value": "Unsupported",
        "Category": "General",
        "Tab": "System Info",
        "Column": "Left",
    },
    {
        "name": "Read Status",
        "value": "Unsupported platform — privileged timing reads disabled",
        "Category": "General",
        "Tab": "System Info",
        "Column": "Left",
    },
]


def apply_formula(value, formula=None):
    if value is None:
        return EM_DASH
    return formula(value) if callable(formula) else value
