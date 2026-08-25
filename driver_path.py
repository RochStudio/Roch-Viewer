"""Where to find the low-level access driver.

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
import sys

DLL_NAME = "inpoutx64.dll"


def search_directories():
    """Every directory the driver might be in, nearest first."""
    directories = []
    if getattr(sys, "frozen", False):
        # The executable's own directory: what the user sees.
        directories.append(os.path.dirname(os.path.abspath(sys.executable)))
    directories.append(os.path.dirname(os.path.realpath(__file__)))
    unpacked = getattr(sys, "_MEIPASS", None)
    if unpacked:
        directories.append(unpacked)
    seen, ordered = set(), []
    for directory in directories:
        if directory and directory not in seen:
            seen.add(directory)
            ordered.append(directory)
    return ordered


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
