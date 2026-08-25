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

"""The AMD AM5 backend: UMC timings, the APOB, the SMU and the FCH bus.

Parts of this package follow ZenStates-Core and ZenTimings by irusanov,
both GPL-3.0. Where a register map, a bit field or a bound comes from
that work it is credited in the module that uses it. See
THIRD_PARTY_NOTICES.txt.
"""
