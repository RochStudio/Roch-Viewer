# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This file follows ZenStates-Core and ZenTimings by irusanov
# (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
# and the bounds applied to decoded values were taken from or checked against
# that work, and the comments below say where. Copyright in those parts
# remains with their authors; this file is distributed under the same licence
# they are, which is what makes that use permitted.
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

"""Power/current limit definitions for the AM5 Power view.

Read-only counterpart to the ZenStates limits page: the same PPT / TDC / EDC /
Scalar figures, but this project never writes them.

Deliberately free of any hardware-access import so the UI row builder and the
tests can use it without pulling in ctypes/InpOut.
"""

import math
from dataclasses import dataclass


@dataclass(frozen=True)
class PowerMetric:
    """One displayable limit plus the range that makes a reading believable."""

    key: str
    label: str
    unit: str
    min_value: float
    max_value: float
    decimals: int = 1


# Rendered top to bottom in this order, matching the ZenStates Power page.
METRICS = (
    PowerMetric("ppt", "PPT", "W", 1.0, 1000.0),
    PowerMetric("tdc", "TDC", "A", 1.0, 1000.0),
    PowerMetric("edc", "EDC", "A", 1.0, 1000.0),
    PowerMetric("scalar", "Scalar", "x", 0.1, 10.0, decimals=2),
)

METRICS_BY_KEY = {metric.key: metric for metric in METRICS}


def validate_power(metric, value):
    """Return the reading, or raise ValueError when it is not believable."""
    value = float(value)
    if not math.isfinite(value):
        raise ValueError("%s is not finite" % metric.label)
    if not (metric.min_value <= value <= metric.max_value):
        raise ValueError(
            "%s %.3f is outside %.2f-%.2f"
            % (metric.label, value, metric.min_value, metric.max_value)
        )
    return value


def format_power(metric, value, limit=None):
    """Render "current / limit unit", or just the limit when there is no value.

    ZenStates shows the configured limit alone. Showing the live draw next to
    it costs nothing here and is what a tuner actually watches, so both appear
    when the value offset is known.
    """
    pattern = "%%.%df" % metric.decimals
    if value is None and limit is None:
        return None
    if value is None:
        return ("%s %s" % (pattern, "%s")) % (limit, metric.unit)
    if limit is None:
        return ("%s %s" % (pattern, "%s")) % (value, metric.unit)
    return ("%s / %s %s" % (pattern, pattern, "%s")) % (
        value, limit, metric.unit
    )
