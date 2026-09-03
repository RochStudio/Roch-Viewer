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

"""The one place the product name and version are written.

Both are shown in the window title and stamped into the executable's file
properties, so a build can be identified from the binary alone without the
directory it came from.
"""

APP_NAME = "Roch Viewer"
__version__ = "1.0.1"

# Windows file-version resources want four numbers.
VERSION_TUPLE = tuple(int(part) for part in __version__.split(".")) + (0,)
