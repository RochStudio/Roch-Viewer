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

r"""Where to find the low-level access driver.

The driver is not distributed with this project -- see THIRD_PARTY_NOTICES --
so it is wherever the person running the tool put it. That is a different
place depending on how the tool is running:

    from source   beside the modules, which is where the README says to put it
    frozen        beside the executable, because that is the directory a user
                  can actually see and drop a file into

A frozen build unpacks its own data into a temporary directory, and looking
only there is what the first version did: it worked while the driver was
bundled and broke the moment it was not, reporting the DLL missing from a
path under AppData\Local\Temp that no one had ever put anything into. That
directory is still searched last, so a build that does choose to bundle the
driver keeps working.
"""

import os

from rochviewer.paths import (
    bundle_directory,
    dedupe,
    frozen_directory,
    module_chain,
)

DLL_NAME = "inpoutx64.dll"


def search_directories():
    """Every directory the driver might be in, nearest first."""
    # The module's directory alone is what this used to be, and after the
    # move into packages that was rochviewer/hardware -- while the README
    # says to put the driver beside run_viewer.py, at the project root.
    # Nothing raised: find_driver returned None and every register-backed
    # reading quietly became N/A.
    return dedupe(
        [frozen_directory()]
        + module_chain(__file__)
        + [bundle_directory()]
    )


def find_driver(name=DLL_NAME):
    """The driver's full path, or None when it is not in any of them."""
    for directory in search_directories():
        candidate = os.path.join(directory, name)
        if os.path.exists(candidate):
            return candidate
    return None


def missing_message(name=DLL_NAME):
    """What to tell someone who has not put the driver anywhere yet."""
    return (
        "%s not found. It is a third-party component and is not distributed "
        "with Roch Viewer; download it from www.highrez.co.uk and put it in "
        "one of:\n  %s"
        % (name, "\n  ".join(search_directories()))
    )
