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


"""Directories a file that ships with the project might be in.

Two things are looked up this way -- the low-level driver and the icon -- and
both were written as "beside my own module". That was the project root until
the modules moved into packages, and afterwards neither was ever found: the
icon fell back to Tk's default, and the driver went missing entirely, which
turns every register-backed reading into N/A while the app still starts and
looks healthy.

The lookups differ in where a bundled copy ranks, so they build their own
order. What they share is this: a module cannot assume the project root is
its own directory, so walk up.
"""

import os
import sys

# Package depth, and why walking up this far is enough: rochviewer/paths.py
# is one level down, rochviewer/ui/main.py two. Going one further than the
# deepest module means the project root is always reached.
PARENT_LEVELS = 3


def module_chain(module_file, levels=PARENT_LEVELS):
    """A module's own directory, then its parents, nearest first.

    The project root is in here wherever the module sits, so moving a module
    between packages cannot silently stop an asset being found.
    """
    directory = os.path.dirname(os.path.realpath(module_file))
    chain = [directory]
    for _ in range(levels - 1):
        parent = os.path.dirname(directory)
        if parent == directory:  # reached the drive root
            break
        chain.append(parent)
        directory = parent
    return chain


def frozen_directory():
    """The executable's own directory when frozen, else None.

    What the user can see and drop a file into.
    """
    if getattr(sys, "frozen", False):
        return os.path.dirname(os.path.abspath(sys.executable))
    return None


def bundle_directory():
    """Where a frozen build unpacks its own data, else None."""
    return getattr(sys, "_MEIPASS", None)


def dedupe(directories):
    """The same list with blanks and repeats dropped, order kept."""
    seen, ordered = set(), []
    for directory in directories:
        if directory and directory not in seen:
            seen.add(directory)
            ordered.append(directory)
    return ordered
