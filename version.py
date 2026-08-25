"""The one place the product name and version are written.

Both are shown in the window title and stamped into the executable's file
properties, so a build can be identified from the binary alone without the
directory it came from.
"""

APP_NAME = "Roch Viewer"
__version__ = "1.0.0"

# Windows file-version resources want four numbers.
VERSION_TUPLE = tuple(int(part) for part in __version__.split(".")) + (0,)
