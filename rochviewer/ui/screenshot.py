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

"""Save a picture of the window, the way timings get shared.

Nothing here needs Pillow. The window is captured through GDI, encoded as a
PNG by hand -- the format is a signature and three chunks around a zlib
stream -- and put on the clipboard as a device-independent bitmap. Each step
takes and returns plain bytes, so everything but the capture itself can be
checked without a display.
"""

import ctypes
import os
import struct
import zlib
from ctypes import wintypes

FOLDER_NAME = "Roch Viewer"


def screenshot_filename(tab_name, when):
    """RochViewer-Summary-20260928-153012.png: the tab, then the moment."""
    tab = "".join(ch for ch in str(tab_name or "") if ch.isalnum()) or "Window"
    return "RochViewer-%s-%s.png" % (tab, when.strftime("%Y%m%d-%H%M%S"))


def pictures_folder():
    """The user's Pictures folder, wherever it has been moved to.

    Asked of the shell rather than built from the profile path: Pictures is
    often redirected into OneDrive, and ~/Pictures is then an empty folder
    nobody looks in.
    """
    try:
        folder_id = _guid("{33E28130-4E1E-4676-835A-98395C3BC3BB}")
        path = ctypes.c_wchar_p()
        result = ctypes.windll.shell32.SHGetKnownFolderPath(
            ctypes.byref(folder_id), 0, None, ctypes.byref(path))
        try:
            if result == 0 and path.value:
                return path.value
        finally:
            # The call allocates the buffer even when it fails, and the
            # caller frees it either way.
            ctypes.windll.ole32.CoTaskMemFree(path)
    except Exception:
        pass
    return os.path.join(os.path.expanduser("~"), "Pictures")


def default_folder():
    return os.path.join(pictures_folder(), FOLDER_NAME)


def encode_png(width, height, bgra):
    """Encode top-down 32-bit BGRA pixels as an RGB PNG."""
    if len(bgra) != width * height * 4:
        raise ValueError("expected %d bytes of pixels, got %d"
                         % (width * height * 4, len(bgra)))
    # BGRA to RGB, one scanline at a time, each led by filter type 0. The
    # alpha GDI hands back is not meaningful for a window capture, so it is
    # dropped rather than written as a transparent image.
    rgb = bytearray(width * height * 3)
    rgb[0::3] = bgra[2::4]
    rgb[1::3] = bgra[1::4]
    rgb[2::3] = bgra[0::4]
    stride = width * 3
    raw = b"".join(
        b"\x00" + bytes(rgb[row * stride:(row + 1) * stride])
        for row in range(height)
    )

    def chunk(kind, data):
        body = kind + data
        return (struct.pack(">I", len(data)) + body
                + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF))

    header = struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0)
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", header)
            + chunk(b"IDAT", zlib.compress(raw, 6)) + chunk(b"IEND", b""))


def save_png(folder, name, width, height, bgra):
    """Write the PNG and return its full path."""
    os.makedirs(folder, exist_ok=True)
    path = os.path.join(folder, name)
    with open(path, "wb") as image:
        image.write(encode_png(width, height, bgra))
    return path


# --- GDI ---------------------------------------------------------------

class _BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", wintypes.DWORD), ("biWidth", wintypes.LONG),
        ("biHeight", wintypes.LONG), ("biPlanes", wintypes.WORD),
        ("biBitCount", wintypes.WORD), ("biCompression", wintypes.DWORD),
        ("biSizeImage", wintypes.DWORD), ("biXPelsPerMeter", wintypes.LONG),
        ("biYPelsPerMeter", wintypes.LONG), ("biClrUsed", wintypes.DWORD),
        ("biClrImportant", wintypes.DWORD),
    ]


class _GUID(ctypes.Structure):
    _fields_ = [("Data1", wintypes.DWORD), ("Data2", wintypes.WORD),
                ("Data3", wintypes.WORD), ("Data4", ctypes.c_ubyte * 8)]


def _guid(text):
    hex_digits = text.strip("{}").replace("-", "")
    raw = bytes.fromhex(hex_digits)
    guid = _GUID()
    guid.Data1, guid.Data2, guid.Data3 = struct.unpack(">IHH", raw[:8])
    guid.Data4[:] = raw[8:]
    return guid


def _bitmap_header(width, height):
    header = _BITMAPINFOHEADER()
    header.biSize = ctypes.sizeof(_BITMAPINFOHEADER)
    header.biWidth = width
    header.biHeight = height
    header.biPlanes = 1
    header.biBitCount = 32
    header.biCompression = 0  # BI_RGB
    return header


PW_RENDERFULLCONTENT = 0x2
SRCCOPY = 0x00CC0020


def _gdi():
    """user32 and gdi32 with every handle typed as a handle.

    Left untyped, ctypes passes a handle as a C int, and a 64-bit handle
    above 2**31 fails with "int too long to convert" -- on some runs and
    not others, depending on where Windows happened to put it.
    """
    user32 = ctypes.windll.user32
    gdi32 = ctypes.windll.gdi32
    H, HDC, HBMP = wintypes.HWND, wintypes.HDC, wintypes.HBITMAP
    user32.GetWindowRect.argtypes = [H, ctypes.POINTER(wintypes.RECT)]
    user32.GetWindowDC.argtypes = [H]
    user32.GetWindowDC.restype = HDC
    user32.GetDC.argtypes = [H]
    user32.GetDC.restype = HDC
    user32.ReleaseDC.argtypes = [H, HDC]
    user32.PrintWindow.argtypes = [H, HDC, wintypes.UINT]
    gdi32.CreateCompatibleDC.argtypes = [HDC]
    gdi32.CreateCompatibleDC.restype = HDC
    gdi32.CreateCompatibleBitmap.argtypes = [HDC, ctypes.c_int, ctypes.c_int]
    gdi32.CreateCompatibleBitmap.restype = HBMP
    gdi32.SelectObject.argtypes = [HDC, wintypes.HGDIOBJ]
    gdi32.SelectObject.restype = wintypes.HGDIOBJ
    gdi32.BitBlt.argtypes = [HDC, ctypes.c_int, ctypes.c_int, ctypes.c_int,
                             ctypes.c_int, HDC, ctypes.c_int, ctypes.c_int,
                             wintypes.DWORD]
    gdi32.GetDIBits.argtypes = [HDC, HBMP, wintypes.UINT, wintypes.UINT,
                                ctypes.c_void_p, ctypes.c_void_p,
                                wintypes.UINT]
    gdi32.DeleteObject.argtypes = [wintypes.HGDIOBJ]
    gdi32.DeleteDC.argtypes = [HDC]
    return user32, gdi32


def capture_window(hwnd):
    """Return (width, height, top-down BGRA bytes) for one window.

    PrintWindow asks the window to draw itself into a bitmap, so a window
    lying over this one -- Telemetry, say -- does not end up in the picture.
    Where a window will not draw that way the screen under it is copied
    instead, which is what it looks like to the person pressing the button.
    """
    user32, gdi32 = _gdi()

    rect = wintypes.RECT()
    if not user32.GetWindowRect(hwnd, ctypes.byref(rect)):
        raise OSError("GetWindowRect failed")
    width, height = rect.right - rect.left, rect.bottom - rect.top
    if width <= 0 or height <= 0:
        raise OSError("the window has no area to capture")

    window_dc = user32.GetWindowDC(hwnd)
    memory_dc = gdi32.CreateCompatibleDC(window_dc)
    bitmap = gdi32.CreateCompatibleBitmap(window_dc, width, height)
    previous = gdi32.SelectObject(memory_dc, bitmap)
    try:
        drawn = user32.PrintWindow(hwnd, memory_dc, PW_RENDERFULLCONTENT)
        if not drawn:
            screen_dc = user32.GetDC(None)
            try:
                gdi32.BitBlt(memory_dc, 0, 0, width, height, screen_dc,
                             rect.left, rect.top, SRCCOPY)
            finally:
                user32.ReleaseDC(None, screen_dc)
        # A negative height asks GetDIBits for rows top to bottom, which is
        # the order the PNG wants them in.
        header = _bitmap_header(width, -height)
        pixels = ctypes.create_string_buffer(width * height * 4)
        gdi32.GetDIBits(memory_dc, bitmap, 0, height, pixels,
                        ctypes.byref(header), 0)
        return width, height, pixels.raw
    finally:
        gdi32.SelectObject(memory_dc, previous)
        gdi32.DeleteObject(bitmap)
        gdi32.DeleteDC(memory_dc)
        user32.ReleaseDC(hwnd, window_dc)


CF_DIB = 8
GMEM_MOVEABLE = 0x0002


def dib_bytes(width, height, bgra):
    """A CF_DIB clipboard payload: header, then the rows bottom to top."""
    header = _bitmap_header(width, height)
    stride = width * 4
    rows = b"".join(bgra[row * stride:(row + 1) * stride]
                    for row in range(height - 1, -1, -1))
    return bytes(header) + rows


def copy_to_clipboard(width, height, bgra):
    """Put the image on the clipboard, for pasting straight into a post."""
    kernel32 = ctypes.windll.kernel32
    user32 = ctypes.windll.user32
    kernel32.GlobalAlloc.restype = wintypes.HGLOBAL
    kernel32.GlobalLock.restype = ctypes.c_void_p
    kernel32.GlobalLock.argtypes = [wintypes.HGLOBAL]
    kernel32.GlobalUnlock.argtypes = [wintypes.HGLOBAL]
    kernel32.GlobalFree.argtypes = [wintypes.HGLOBAL]
    user32.SetClipboardData.argtypes = [wintypes.UINT, wintypes.HANDLE]
    user32.SetClipboardData.restype = wintypes.HANDLE

    payload = dib_bytes(width, height, bgra)
    handle = kernel32.GlobalAlloc(GMEM_MOVEABLE, len(payload))
    if not handle:
        raise OSError("GlobalAlloc failed")
    pointer = kernel32.GlobalLock(handle)
    if not pointer:
        kernel32.GlobalFree(handle)
        raise OSError("GlobalLock failed")
    ctypes.memmove(pointer, payload, len(payload))
    kernel32.GlobalUnlock(handle)
    if not user32.OpenClipboard(None):
        kernel32.GlobalFree(handle)
        raise OSError("the clipboard is in use")
    try:
        user32.EmptyClipboard()
        # Once SetClipboardData succeeds the clipboard owns the memory; it
        # is only freed here when the handover failed.
        if not user32.SetClipboardData(CF_DIB, handle):
            kernel32.GlobalFree(handle)
            raise OSError("SetClipboardData failed")
    finally:
        user32.CloseClipboard()
