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

"""Where to find a bundled asset such as the application icon.

An asset sits at the project root when running from source and is unpacked
into the temporary bundle directory when frozen, so neither location alone
is enough. The version this replaced looked only beside its own module,
which was the project root until the modules moved into packages -- after
that it silently found nothing, and the window fell back to Tk's default
icon in the taskbar while the EXE's own file icon, baked in at build time,
still looked correct.
"""

import os

from rochviewer.paths import (
    bundle_directory,
    dedupe,
    frozen_directory,
    module_chain,
)

ICON_NAME = "icon.ico"


def search_directories():
    """Every directory an asset might be in, nearest first.

    The bundle is searched before the module chain here, unlike the driver:
    an asset is something the project ships, so a bundled copy is the right
    one, where the driver is something the user supplies.
    """
    return dedupe(
        [frozen_directory(), bundle_directory()] + module_chain(__file__)
    )


def find_asset(name):
    """An asset's full path, or None when it is in none of them."""
    for directory in search_directories():
        candidate = os.path.join(directory, name)
        if os.path.exists(candidate):
            return candidate
    return None


def find_icon():
    """The application icon's full path, or None."""
    return find_asset(ICON_NAME)
