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

import customtkinter as ctk
from rochviewer.paths import module_chain
from rochviewer.ui.asset_path import find_icon
from rochviewer.ui.lazy_read import read_timing
from rochviewer.timings import TIMINGS, apply_formula
from rochviewer.memory.dimm_inventory import channel_of, read_modules
from rochviewer.version import APP_NAME, __version__
from rochviewer.ui.display_values import (
    WINDOWED_TABS,
    is_dual_timing,
    resolve_display_value,
    select_tab_names,
)
import base64
import ctypes
import json
import os
import sys
import struct
import threading
import tkinter
import warnings
import webbrowser
import wmi


# Termination and drive rows: their own Summary panel, never a timing column.
SIGNAL_CATEGORIES = ("RTT", "ODT", "RON", "Drive Strength")


def summary_signal_timings(timings):
    """Select the RTT/ODT/RON/Drive Strength rows for the Summary signal panel."""
    return [
        timing for timing in timings
        if timing.get("Category") in SIGNAL_CATEGORIES
    ]


# Terminations that are switched off, which read as zero ohms. RTT_OFF is the
# Intel table's spelling; Hi-Z is the same state named after the impedance.
# Compared with spaces removed and upper-cased, so "Rtt_off" and "Hi Z"
# match too. "DISABLED" is what the DDR4 RTT tables emit for code 0 --
# without it those rows fell through to the verbatim branch and the
# Summary printed "Disabled/Disabled", which does not fit the column and
# says in nine characters what 0 says in one.
RTT_OFF_VALUES = ("RTT_OFF", "OFF", "HI-Z", "HIZ", "DISABLED")

# Categories whose rows are drivers rather than terminations. Switching a
# termination off leaves the line unterminated, and this panel has always
# written that as zero. Switching a *driver* off is the opposite thing: the
# pin stops driving and goes high impedance, so printing zero ohms there says
# it is shorted to the rail rather than released.
DRIVER_CATEGORIES = ("RON", "Drive Strength")


def summary_rtt_display(value, category=None):
    """Summary-only: the ohms alone, as a bare number.

    Every row in this block is a resistance, so the column reads as numbers
    down the page -- 40/40, 0/0, 48/48 -- rather than as RZQ ratios with the
    figure buried in brackets. The unit is dropped with the ratio: it is the
    same for every row, and repeating it costs width the pairs need.

      "RZQ/6 (40 Ω)"  -> "40"
      "RZQ/4 (60)"    -> "60"       (Intel tables omit the unit)
      "RZQ/0.5 (480)" -> "480"
      "RTT_OFF"       -> "0"        (an unterminated line)
      "Disabled"      -> "0"        (the DDR4 tables' word for it)
      "40 Ω"          -> "40"
      "RFU"           -> "RFU"      (reserved: not a resistance)

    On a drive-strength row the off state keeps its name instead:

      "Hi-Z"          -> "Hi-Z"     (a driver that is not driving)
      "Off"           -> "Hi-Z"
      "Disabled"      -> "Hi-Z"

    The Timings tab keeps the full RZQ string, where the ratio is the point.
    """
    text = "" if value is None else str(value).strip()
    if not text or text == "—":
        return text
    if text.upper().replace(" ", "") in RTT_OFF_VALUES:
        return "Hi-Z" if category in DRIVER_CATEGORIES else "0"

    start = text.rfind("(")
    end = text.rfind(")")
    if start != -1 and end > start:
        text = text[start + 1:end].strip()
    # Whatever is left may still carry the unit, from either source.
    bare = text.replace("Ω", "").replace("ohm", "").replace("Ohm", "").strip()
    if bare and bare.replace(".", "", 1).isdigit():
        return bare
    return text


def _grid_padx(info):
    """Total horizontal padding a grid_info reports, as a number.

    Tk gives padx back as one value or as a (left, right) pair, and the pair
    form is what the Summary strip uses, so a caller that assumed a number
    silently measured zero for exactly the cells that had gaps.
    """
    padx = info.get("padx", 0)
    if isinstance(padx, (tuple, list)):
        return sum(int(side or 0) for side in padx)
    try:
        return 2 * int(padx or 0)
    except (TypeError, ValueError):
        return 0


def summary_compact_ohm(value):
    """Tighten ohm spacing for slash-paired Summary values (40Ω/40Ω)."""
    text = "" if value is None else str(value).strip()
    if not text:
        return text
    return (
        text.replace(" Ω", "Ω")
        .replace(" ohm", "Ω")
        .replace(" Ohm", "Ω")
    )


def summary_slash_pair(value_a, value_b):
    """Format dual-channel Summary values as ChA/ChB slash pairs."""
    left = summary_compact_ohm(value_a)
    right = summary_compact_ohm(value_b)
    if not left:
        left = "—"
    if not right:
        right = "—"
    return f"{left}/{right}"


def channel_slot_labels(modules=None):
    """Name the two channel columns after the slot each one holds.

    "A2/B2" says which stick a column is; "ChA/ChB" only says which memory
    controller. The slot comes from the board's own DeviceLocator, so it is
    the same name printed on the board and shown by every other tool.

    Only a channel holding exactly one module is renamed. With two sticks on a
    channel the column still covers both, and neither slot name would be
    right, so that channel keeps its generic label.
    """
    try:
        from rochviewer.memory.dimm_inventory import read_modules, slots_by_channel

        grouped = slots_by_channel(read_modules() if modules is None else modules)
    except Exception:
        return {}
    return {
        channel.lower(): slots[0]
        for channel, slots in grouped.items()
        if len(slots) == 1
    }


def summary_system_memory_blocks(available_names):
    """Return ``(names, aligned)`` per Summary About row.

    ``aligned`` puts one entry per Summary column, on those columns, so the
    entry starts at the x the timing section below it starts at. An aligned
    row keeps a hole for a name the platform does not report, because dropping
    it would slide everything after it into the wrong column; the renderer
    leaves that cell empty. A row that is not aligned packs tight, each entry
    starting where the last value ended.
    """
    available = set(available_names)
    blocks = []

    def add(names):
        row = [name for name in names if name in available]
        # The panel configures columns for SUMMARY_PAIRS_PER_ROW label/value
        # pairs. A longer row wraps rather than spilling past the last one into
        # the filler, which is what pushed a fourth entry against the right
        # edge while the rest of the row stayed bunched left.
        for start in range(0, len(row), SUMMARY_PAIRS_PER_ROW):
            blocks.append((row[start:start + SUMMARY_PAIRS_PER_ROW], False))

    def add_aligned(names):
        row = [name if name in available else None for name in names]
        if any(row):
            blocks.append((row, True))

    add(("CPU", "Cores / Threads"))

    if "AGESA" in available:
        # AGESA is the AM5 marker: Intel reports Microcode in its place. This
        # used to also accept MbVendor, which stopped meaning anything when
        # the board rows were renamed and left the test looking satisfied.
        #
        # Identity first, packed tight so a long board name stays readable,
        # then a block that reads down the columns:
        #
        #   what the DRAM runs at   what the firmware is   the clocks
        #   DRAM Frequency          AGESA                  MCLK
        #   DRAM Ratio              BCLK                   FCLK
        #   UCLK:MCLK               Memory Capacity        UCLK
        #   Power Down Mode         Refresh Mode           Nitro
        #   Gear Down Mode
        #
        # Each column starts where the timing section under it does, so the
        # memory picture sits over tCL, the firmware over tREFI, and the
        # clocks over RTT WR.
        #
        # DRAM Ratio leads UCLK:MCLK because it is the coarser of the two: the
        # ratio the kit is running at, then how the controller is geared to
        # it. The first column is one row longer than the others as a result,
        # and Gear Down Mode takes that row alone rather than the other two
        # columns being padded to reach it.
        # Model, the name System Info gives the board row. The vendor is
        # not carried here: the model names itself, and the two together
        # spent a third of the strip on one fact.
        add(("Model", "BIOS"))
        add_aligned(("DRAM Frequency", "AGESA", "MCLK"))
        add_aligned(("DRAM Ratio", "BCLK", "FCLK"))
        add_aligned(("UCLK:MCLK", "Memory Capacity", "UCLK"))
        add_aligned(("Power Down Mode", "Refresh Mode", "Nitro Rx/Tx/Ctrl"))
        add_aligned(("Gear Down Mode", None, None))
        return blocks

    # The memory picture reads down the first column: the speed the DRAM
    # runs at, how the controller is geared to it, and whether it is
    # allowed to power down. The clocks derived from that speed line up on
    # the right, MCLK above UCLK.
    #
    # Aligned like the AM5 block, so each column starts where the timing
    # section under it does: DRAM Frequency over tCL, BCLK over tREFI, MCLK
    # over RTT WR. Packing tight instead let each row pick its own column
    # positions, so the three rows stepped in and out against the grid below
    # them.
    #
    # FCLK is not in these rows because the Intel backend does not report one
    # -- it is an AM5 clock, and the AM5 block above places it. Adding a name
    # here that never resolves would cost a permanent hole in the row.
    #
    # Microcode sits with BIOS: both are firmware revisions, and the pairing
    # reads better than a CPU fact stranded in the middle of the memory
    # block. The rows under it come up one each so the hole it leaves lands
    # at the foot of the column rather than in the middle of it.
    add(("Model", "BIOS", "Microcode"))
    add_aligned(("DRAM Frequency", "BCLK", "MCLK"))
    add_aligned(("Gear Mode", "Memory Capacity", "Uncore"))
    # Power Down is the Misc tab's row, read from the controller's own bit.
    # AM5 above still places Power Down Mode, which is its own reading from
    # its own profile -- the two names are not interchangeable.
    add_aligned(("Power Down", None, "UCLK"))
    if "Gear Down Mode" in available:
        add(("Gear Down Mode", "Nitro Rx/Tx/Ctrl"))
    return blocks


def summary_system_memory_layout(available_names):
    """Return the Summary About layout as plain rows of names."""
    return [
        [name for name in names if name]
        for names, _aligned in summary_system_memory_blocks(available_names)
    ]


# Label/value pairs the Summary system panel lays out per row. The panel
# configures exactly this many column pairs plus one filler, so the layout and
# the grid have to agree.
SUMMARY_PAIRS_PER_ROW = 3

SUMMARY_BASE_COLUMNS = 3

# Tabs whose generic sections get the Summary's zebra shading. System Info
# reads as one long list with a wide gap between each name and its value,
# which is exactly the case the banding was added for. The timing tabs put
# their values close to their names and are already split into short
# sections, so shading them would be noise.
SHADED_TABS = frozenset({"System Info", "Timings", "Skew", "Misc"})

# Tabs drawn as one continuous table per column rather than a stack of blocks:
# the A1/B1 header once at the top, the section names as banded rows, and no
# padding between sections. Every row is then the same pitch from the same
# origin, so the two columns line up and take the same shade at the same row
# without anything having to force them to.
CONTINUOUS_SECTION_TABS = frozenset({"Timings", "Skew", "Misc"})

# Tabs whose two halves are instead padded to a shared row grid so a band runs
# unbroken across both. That padding is dead space -- a section is brought up
# to the height of the one facing it, which on Timings meant nine blank rows
# under Primary, seven rows against Tertiary's sixteen -- so it is only worth
# it where the facing sections are within a row or two of each other.
#
# Derived rather than listed: a continuous tab already lines its columns up, so
# padding one would add blank rows to a layout that does not need them. The two
# sets are alternatives, and deriving one from the other keeps them that way.
PAIRED_SECTION_TABS = SHADED_TABS - CONTINUOUS_SECTION_TABS

# The Timings tab's section order: the timing groups down the left, read in
# the order a training result is read, and the signal and mode groups down
# the right. Section order otherwise follows the order rows happen to appear
# in the profile, which is not an order anyone chose.
#
# Which column a section lands in comes from its rows' Column field, not from
# here; this only orders what is already in a column. An unlisted section
# sorts to the foot of its column, which is why Tertiary and Other Timings are
# named: without them the Intel right-hand column read Power down before
# Tertiary, and the left-hand one only reached Other Timings by accident.
# One list, sorted into each column separately, so only the order of sections
# *within* a column matters and the list as a whole need not read as a layout.
#
# It cannot read as one, because both platforms share it and they do not agree
# on which column a section belongs to. Power down is the case that forces the
# issue: AM5 puts it on the left under CAS to CAS, Intel on the right under
# Tertiary. So this is one sequence satisfying four column orders at once:
#
#   AM5 left     Primary, Secondary, CAS to CAS, Power down, Stagger,
#                Preamble / postamble, Mode register
#   AM5 right    Refresh timings, Turnaround, Read to read, Write to write,
#                PHY
#   Intel left   Primary, Secondary, Other Timings, Command
#   Intel right  Refresh timings, Tertiary, Power down
#
# Which is why Power down sits far from CAS to CAS here despite following it
# on AM5: it also has to trail Tertiary for Intel, and one position has to do
# both. Moving a name in this list means checking all four, and the tests do.
TIMINGS_SECTION_ORDER = (
    "Primary",
    "Secondary",
    "Other Timings",
    "Command",
    "CAS to CAS",
    "Refresh timings",
    "Tertiary",
    "Turnaround",
    "Read to read",
    "Write to write",
    "PHY",
    "Power down",
    "Stagger",
    "Preamble / postamble",
    "Mode register",
    # Skew tab sections. This order is only applied to the Timings tab and
    # they no longer appear there, so their position is inert -- but a name
    # dropped from the list is easy to mistake for a section deliberately
    # removed.
    "RTT",
    "ODT",
    "Drive Strength",
)


def ordered_sections(sections, order):
    """Sort (category, rows) pairs into `order`.

    A category that is not listed keeps its position relative to the other
    unlisted ones and follows the listed ones, so a section added to the
    profile later still appears rather than vanishing from the tab.
    """
    rank = {name: index for index, name in enumerate(order)}
    return sorted(sections, key=lambda item: rank.get(item[0], len(rank)))

# How often rows marked ``live`` are re-read while the window is open. The
# runtime caches a successful read for slightly less than this, so one tick
# costs one read per transport rather than one per row.
LIVE_REFRESH_MS = 1000

# What a row shows when it could not be read. Blank would read as zero.
EM_DASH = "—"

# How long after startup the rows that came back empty are read once more.
# Long enough for the opening burst of reads to be over, since contention
# for the same mailbox is what blanked them.
BLANK_RETRY_MS = 1500

# Sensor Telemetry group order: what the silicon is doing first, then what it
# is drawing, then the rails feeding it. The rails are one section, ordered
# core outward within it. The per-DIMM panels follow, each with its own PMIC.
SENSOR_GROUP_ORDER = (
    "Clocks",
    "Thermal & Power",
    "Voltages",
    "Graphics",
    # Last, because it is the one section that should stay empty of news.
    "Errors",
)

# The Summary is one continuous table. Gaps between groups were tried and
# removed: every column broke at a different row, which put the columns out of
# step by the height of a gap and made a shaded row impossible to carry across
# the width. The shading does the grouping instead.


def summary_voltage_names(timings):
    """Return the rail rows, in the order the platform declares them.

    Filtering is on rail identity rather than display label, so renaming a rail
    can never change which rows are selected.  See
    voltage_rails.SUMMARY_HIDDEN_RAILS for the rails held back.
    """
    from rochviewer.sensors.voltage_rails import SUMMARY_HIDDEN_RAILS

    return [
        timing.get("name") for timing in timings
        if timing.get("rail_key")
        and timing.get("rail_key") not in SUMMARY_HIDDEN_RAILS
    ]


def summary_column_count(_timings=None):
    """Summary is three columns: timings, timings, termination.

    A fourth used to carry the rails. They read one instant each, which is the
    least useful thing to know about a voltage, so they live in the Sensor
    Telemetry window now with their minimum, maximum and average.
    """
    return SUMMARY_BASE_COLUMNS


def half_is_used(frame):
    """True when a tab half actually holds sections.

    Managed is not the same as used. Misc grids a right half like every other
    tab and then puts nothing in it, so a check for "is it managed" took the
    two-column branch: the left was sized to its own content and the rest of
    the tab went to an empty frame, leaving the bands stopping a third of the
    way across. A half with no children is the single-column case, which is
    what System Info has always been.

    Written to answer for anything with the two attributes rather than for a
    widget specifically, so the rule can be tested without standing up a
    window.
    """
    if frame is None:
        return False
    manager = getattr(frame, "winfo_manager", None)
    children = getattr(frame, "winfo_children", None)
    if manager is None or children is None:
        return False
    try:
        return bool(manager()) and bool(children())
    except Exception:
        return False


def summary_column_width(width, is_last, gap=0):
    """Round a Summary column up to a width its row shading can fill.

    CustomTkinter's draw engine rounds a fill down to an even number of
    pixels. A column of odd width therefore leaves its own last pixel
    unpainted, and since the columns tile with no gutter that unpainted
    pixel reads as a hairline running down the tab -- the gap that showed
    between tRDWR's value and RTT Nom WR, where a 177px column met the next
    one. Only a column with a neighbour to its right can show it; the last
    column ends at the panel edge, where there is nothing to divide from.

    ``gap`` is trailing space inside the column, so its content stops
    short of the next column rather than running into it: the middle
    and right columns were a pixel apart. Inside the column rather than
    between them, because they tile with no gutter and a gutter would
    break every band into three.
    """
    if is_last:
        return width
    width += gap
    return width + width % 2


def summary_system_memory_names():
    """Return the rows eligible for the full-width Summary system panel."""
    # Eligibility, not placement: summary_system_memory_layout decides which of
    # these actually appear and where. Refresh Mode stays listed because the
    # AM5 layout still places it; the Intel layout does not, since it now heads
    # the Refresh timings section on the Timings tab next to the tREFI it
    # governs.
    return [
        "CPU", "Cores / Threads",
        "Model", "BIOS", "AGESA", "Microcode",
        "BCLK", "Uncore", "FCLK", "MCLK", "UCLK", "DRAM Frequency",
        "DRAM Ratio", "UCLK:MCLK", "Refresh Mode", "Gear Mode",
        "Memory Capacity",
        "Power Down Mode", "Power Down", "Gear Down Mode", "Nitro Rx/Tx/Ctrl",
    ]


# DDR5 mode-register VREF rows. Summary leaves these out when the platform
# also reports the up/down levels: they read as a percentage and a pair of
# cross-references ("50.0% (fixed)", "Uses CA VREF") rather than a level that
# can be compared with the rows around them.
#
# Arrow Lake is the exception and the reason this is a filter rather than a
# fixed list: it drops the up/down block entirely, so there these three are the
# only VREF readings there are and they stay.
DDR5_MODE_REGISTER_VREF = ("DQ VREF", "CA VREF", "CS VREF")

# The analog up/down block, matched by name so its presence can be detected.
# These rows were called DQ VREFUP, CMD VREFDN and so on until they were
# renamed to the reference tools' names for the same bits -- WrDS for write
# drive strength, RdODT for read ODT. The names changed; the registers and
# the bit fields did not.
VREF_LEVEL_PREFIXES = ("WrDS", "RdODT")

# VREF rows the Summary leaves to the Skew tab.
#
# DQ VREF is per DRAM device, so it is four rows rather than one, and four
# device levels say nothing a Summary reader wants: the up/down pair above
# already gives the level, and which device differs by half a percent is a
# tuning detail. RX VREF is a receiver reference on a different scale from
# every other row in the panel, and QXCOUNT is a comparator count rather than
# a voltage at all -- it sits with the VREF rows on Skew because that is what
# it belongs to, not because it is one.
SUMMARY_EXCLUDED_VREF_PREFIXES = ("DQ VREF D", "RX VREF", "QXCOUNT")

# Primary/Secondary rows Summary leaves to the Timings tab. The whole tCCD
# group is here: it is four rows off one mode-register nibble, which is a lot
# of Summary space for one setting. Named rather than written inline because
# an earlier version of this list said "tCCDL" and "tCCDL WR", which no row
# has ever been called, so the exclusion matched nothing and every one of them
# showed anyway.
SUMMARY_EXCLUDED_TIMING_NAMES = (
    "tCCD_L", "tCCD_L_WR", "tCCD_L_WR2", "tREFI",
    # One spelling on both platforms now; AM5 always called it CR.
    "CR",
    # A back-to-back allowance rather than a timing anyone reads at a glance,
    # and it landed in the Tertiary column between tRDRD_sg and tRDRD_dg where
    # it broke up the group. Still on the Timings tab.
    "Allow 2cyc B2B LPDDR",
    # Both restate the row above them on the Timings tab, which is where the
    # pairing is the point. On the Summary the row they restate is already
    # listed, so these only lengthen the column. They stay on Timings,
    # directly under what they restate.
    "tWR_MR", "tRTP_MR",
)


# Carried at the foot of the Summary signal panel, under the VREF levels.
# Added at the call site rather than inside summary_vref_row_names, which
# takes its rows straight from the Skew tab's VREF category so the two
# displays cannot drift: DLL BWSEL is not a VREF row and putting it there
# would mean the function no longer described what it returns.
SUMMARY_SIGNAL_TAIL_ROWS = ("DLL BWSEL",)
SUMMARY_SIGNAL_TAIL_ANCHOR = "WrDSCke CS Up"


def summary_vref_row_names(timings):
    """Return the VREF rows for the Summary signal panel, in table order."""
    names = [
        timing.get("name") for timing in timings
        if timing.get("Category") == "VREF"
        and not str(timing.get("name")).startswith(
            SUMMARY_EXCLUDED_VREF_PREFIXES
        )
    ]
    if any(str(name).startswith(VREF_LEVEL_PREFIXES) for name in names):
        return [name for name in names if name not in DDR5_MODE_REGISTER_VREF]
    return names


SUMMARY_RTL_ROWS = (
    ("RTL MC0 C0 A1/A2", "RTL MC0 CHA R0", "RTL MC0 CHA R1"),
    ("RTL MC0 C1 A1/A2", "RTL MC0 CHB R0", "RTL MC0 CHB R1"),
    ("RTL MC1 C0 B1/B2", "RTL MC1 CHA R0", "RTL MC1 CHA R1"),
    ("RTL MC1 C1 B1/B2", "RTL MC1 CHB R0", "RTL MC1 CHB R1"),
)

# Shown directly under the RTL pairs, and written as (label, name) rather
# than the (label, first, second) above. An RTL entry slashes two separate
# rows together; a DFE row already carries both channels, so the pair is its
# own A and B. Summary reads both forms.
SUMMARY_DFE_BIAS_ROWS = tuple(
    (f"DFE Tap {tap} Bias", f"DFE Tap {tap} Bias") for tap in (1, 2, 3, 4)
)


def is_summary_pair(entry):
    """True for either paired form: (label, name) or (label, first, second)."""
    return isinstance(entry, tuple) and len(entry) in (2, 3)


def insert_summary_rows_after(names, anchor, extra):
    """Place extra rows directly below the anchor timing name."""
    rows = list(names)
    try:
        position = rows.index(anchor) + 1
    except ValueError:
        # Anchor missing on this platform: fall back to the end of the column.
        position = len(rows)
    return rows[:position] + list(extra) + rows[position:]


def insert_summary_rtl_after(names, anchor):
    """Place the paired RTL rows, and the DFE bias under them, below anchor."""
    return insert_summary_rows_after(
        names, anchor, SUMMARY_RTL_ROWS + SUMMARY_DFE_BIAS_ROWS
    )


# The Summary's left column, in this order. tRCDRD leads tRCDWR because read
# is the one that gets tuned first, and the power-down pair tails the column
# after tMOD rather than interrupting the precharge and mode-register rows.
AM5_SUMMARY_TIMING_PRIORITY = (
    "tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC", "tWR",
    "tRFCns", "tRFC", "tRFC2", "tRFCsb", "tRRD_L", "tRRD_S", "tWTR_L",
    "tWTR_S", "tRTP", "tFAW", "tCWL", "tRDPRE", "tWRPRE", "tMOD",
    "tCKE", "tXP",
)

# Which rows are held at the foot of the Summary's left column whatever the
# category filters do. Membership only -- the order is the one above.
SUMMARY_COLUMN_TAIL = frozenset({
    "tRDPRE", "tWRPRE", "tMOD", "tCKE", "tXP",
})

# Shown in the Summary system strip, not the timing columns.
AM5_SUMMARY_SYSTEM_ONLY = frozenset({"Refresh Mode"})

# Rows the Summary places itself, which must therefore not also arrive through
# the generic columns. CR is a Timings row so that the Timings tab can close
# its primary group with it, and the Summary puts it under tRC by name -- and
# without this it came through both ways and appeared twice.
AM5_SUMMARY_PLACED_NAMES = frozenset({"CR"})

# Timings rows that stay on the Timings tab and never reach the Summary. Its
# own set rather than a reuse of the two above, because it means neither of
# their things: these are not placed elsewhere in the Summary and are not in
# the system strip, they are simply not summarised.
#
# tREFIns is tREFI restated in nanoseconds. On the Timings tab it earns its
# place next to the raw interval it converts; in a Summary column it is a
# second row saying what the row above it already said.
AM5_SUMMARY_OMITTED = frozenset({"tREFIns"})

AM5_SUMMARY_PHY_NAMES = ("tPHYWRD", "tPHYRDL", "tPHYWRL")

# Pinned to the top of the Summary middle column, in this order: the refresh
# interval, then the same-direction read and write groups, each complete,
# then the two turnarounds. Everything else follows in TIMINGS order.
AM5_SUMMARY_LEFTOVER_PRIORITY = (
    "tREFI",
    "tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD",
    "tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD",
    "tWRRD", "tRDWR",
)


def intel_summary_timing_columns(timings):
    """Return the Intel Summary's (primary/secondary, tertiary) column names.

    SUMMARY_EXCLUDED_TIMING_NAMES governs both columns. It used to be applied
    to the first only, so a name added to it that happened to be a Tertiary
    row -- Allow 2cyc B2B LPDDR was -- stayed on the Summary with nothing to
    say why.
    """
    def wanted(category):
        return [
            timing.get("name") for timing in timings
            if timing.get("Category") == category
            and timing.get("name") not in SUMMARY_EXCLUDED_TIMING_NAMES
        ]

    primary_secondary = wanted("Primary") + wanted("Secondary") + [
        "tCKE", "tXP", "tRDPRE", "tWRPRE", "tMOD",
        # Power-down group, pinned under tMOD. tPRPDEN is deliberately not
        # here: it stays on the Timings tab with the rest of the Power down
        # section.
        "tRDPDEN", "tWRPDEN", "tCPDED",
    ]
    # The refresh cycle times follow the write recovery, which is where they
    # were asked for. tRFC is named both ways because the two generations
    # spell it differently -- DDR5 renames it to tRFC2 -- so on either
    # platform exactly one of that pair exists and the other resolves to
    # nothing. tRFCns is not in that position: it is the same derived row
    # under the same name on both, and tRFCpb only ever exists on DDR5.
    primary_secondary = insert_summary_rows_after(
        primary_secondary, "tWR",
        [name for name in ("tRFCns", "tRFC2", "tRFC", "tRFCpb")
         if any(timing.get("name") == name for timing in timings)],
    )
    # The refresh interval heads the column, ahead of the turnarounds.
    tertiary = ["tREFI", "tREFIx9"] + [
        name for name in wanted("Tertiary")
        if name not in ("tREFI", "tREFIx9", "tCKE")
    ]
    # RTL sits with the turnaround group, directly under tWRWR_dd.
    return primary_secondary, insert_summary_rtl_after(tertiary, "tWRWR_dd")


def am5_summary_timing_columns(timings):
    """Return requested AM5 priority rows, followed by stable leftovers.

    Leftovers keep TIMINGS order except for AM5_SUMMARY_LEFTOVER_PRIORITY,
    which is pinned to the top: the same-direction read and write groups,
    then the two turnarounds, then everything else.
    Dual ChA/ChB PHY rows are excluded here and rendered in their own section.
    """
    # Every Timings-tab row that is not a termination row is a timing, rather
    # than a fixed list of section names. The tertiary block is split into
    # several named sections, and naming them here again would mean a renamed
    # or added section silently vanishing from the Summary.
    phy = set(AM5_SUMMARY_PHY_NAMES)
    available = []
    for timing in timings:
        name = timing.get("name")
        if (
            timing.get("Tab", "Timings") == "Timings"
            and timing.get("Category") not in SIGNAL_CATEGORIES
            and name
            and name not in available
            and name not in phy
        ):
            available.append(name)
    elsewhere = (set(AM5_SUMMARY_SYSTEM_ONLY) | set(AM5_SUMMARY_PLACED_NAMES)
                 | set(AM5_SUMMARY_OMITTED))
    first = [
        name for name in AM5_SUMMARY_TIMING_PRIORITY
        if name in available and name not in elsewhere
    ]
    leftover = [
        name for name in available
        if name not in first and name not in elsewhere
    ]
    pinned = [name for name in AM5_SUMMARY_LEFTOVER_PRIORITY if name in leftover]
    if pinned:
        promoted = set(pinned)
        leftover = pinned + [name for name in leftover if name not in promoted]
    return first, leftover


class TimingGUI:
    def __init__(self, root):
        self.root = root
        self.root.title(f"{APP_NAME} {__version__}")
        self.set_window_icon()
        self.setup_appearance()
        # Summary gains a voltage column only on platforms that report rails.
        self.summary_columns = summary_column_count(TIMINGS)
        # Slot names for the two channel columns, read once. See
        # channel_slot_labels: A2/B2 rather than ChA/ChB when each channel
        # holds a single module.
        self.channel_labels = channel_slot_labels()
        # (timing, label) pairs re-read while the window is open.
        self.live_value_labels = []
        # Fixed rows whose one read came back empty; read once more below.
        self._blank_labels = []
        self._live_refresh_busy = False
        # Dual-channel section bodies per tab, aligned once every tab is built.
        self._dual_content_frames = {}
        # Summary About rows that sit on the timing columns below them.
        self._summary_about_rows = []
        # How many shaded rows a column has drawn, so a headerless section
        # continues the alternation instead of restarting it, and the grid
        # its last row sits in, so the bands can be carried to the foot of
        # the tallest column.
        self._stripe_rows = {}
        self._stripe_tail = {}
        # Section bodies per tab, so the left and right halves of a banded tab
        # can be brought to the same height once the real row pitch is known.
        self._section_bodies = {}
        # Section heading labels on continuous tabs, so their height can be
        # matched to a measured data row once the rows exist.
        self._section_headers = {}
        self.build_title_bar()
        self.create_widgets()
        self.setup_window_geometry()
        self.load_all_tabs_content()
        # The tabs only know how wide they need to be once their rows
        # exist, so both passes run after they are built -- and in this
        # order, because stretching changes the widths the window is
        # then fitted to. Fitted first, the window was sized to what the
        # tabs asked for before they were levelled and ended up wider
        # than the content it was meant to fit.
        self._stretch_tab_halves()
        self._widen_to_fit_tabs()
        self.start_live_refresh()
        self.root.after(BLANK_RETRY_MS, self._retry_blank_values)
        if self.load_settings().get("telemetry_auto_open"):
            # After the window exists, so the pop-out lands on top of it.
            self.root.after(400, self.open_dimm_telemetry)

    # --- Custom title bar.
    #
    # The window draws its own top strip rather than wearing the Windows one,
    # so the chrome follows the app's own light/dark colors instead of sitting
    # in a band that ignores them. There is no maximize control: the layout is
    # fitted to its content and a maximized window only adds empty space.
    #
    # Dropping the native frame drops three things Windows was providing, so
    # each is put back rather than left missing: the taskbar button (an
    # undecorated window is treated as a tool window and hidden from it), the
    # minimize path (iconify does nothing while overrideredirect is set), and
    # the resize border (a grip sits in the footer instead).
    TITLE_BAR_HEIGHT = 30
    TITLE_BUTTON_WIDTH = 44
    CLOSE_HOVER_COLOR = "#C42B1C"

    def build_title_bar(self):
        self.root.overrideredirect(True)
        self._undecorated = True
        self.restore_taskbar_button()

        bar = ctk.CTkFrame(self.root, height=self.TITLE_BAR_HEIGHT,
                           corner_radius=0, fg_color=self.HEADER_COLOR)
        bar.pack(fill="x", side="top")
        bar.pack_propagate(False)
        self.title_bar = bar

        # Packed before the title so it sits to its left, at the head of the
        # strip. Held on self because Tk drops an image nothing refers to.
        self.logo = self.logo_image(self.LOGO_SIZE)
        logo_label = None
        if self.logo is not None:
            # The warning is about HighDPI scaling, which needs CTkImage and
            # so Pillow. Answered above; silenced here so it does not read as
            # something going wrong.
            with warnings.catch_warnings():
                warnings.simplefilter("ignore", UserWarning)
                logo_label = ctk.CTkLabel(bar, image=self.logo, text="")
            logo_label.pack(side="left", padx=(8, 0))

        title = ctk.CTkLabel(
            bar, text=f"{APP_NAME} {__version__}", font=self.COMPACT_BOLD,
            text_color=self.BRAND_COLOR, anchor="w",
        )
        title.pack(side="left", padx=(6, 10))

        for text, command, hover in (
            ("✕", self.root.destroy, self.CLOSE_HOVER_COLOR),
            ("–", self.minimize_window, self.TAB_UNSELECTED_HOVER_COLOR),
        ):
            ctk.CTkButton(
                bar, text=text, command=command,
                width=self.TITLE_BUTTON_WIDTH, height=self.TITLE_BAR_HEIGHT,
                corner_radius=0, fg_color="transparent", hover_color=hover,
                text_color=self.TEXT_COLOR, font=self.COMPACT_BOLD,
            ).pack(side="right")

        # The strip, its title and the logo all drag: a widget sitting on the
        # bar would otherwise be a dead patch in the middle of it.
        for widget in (bar, title, logo_label) if logo_label else (bar, title):
            widget.bind("<Button-1>", self.start_window_drag)
            widget.bind("<B1-Motion>", self.drag_window)

        # Restoring from the taskbar re-maps the window, which is where the
        # native frame comes back off. Guarded on the widget because <Map>
        # fires for every child as the tabs are built, not only for the
        # window itself.
        self.root.bind(
            "<Map>",
            lambda event: (event.widget is self.root
                           and self.restore_from_taskbar()),
            add="+",
        )

    def restore_taskbar_button(self):
        """Put the window back on the taskbar after the frame comes off.

        Windows hides undecorated windows from the taskbar by treating them as
        tool windows. Clearing that bit and setting the app-window one puts the
        button back; the hide/show is what makes the change take effect.
        """
        try:
            gwl_exstyle, ws_ex_appwindow, ws_ex_toolwindow = -20, 0x40000, 0x80
            self.root.update_idletasks()
            hwnd = ctypes.windll.user32.GetParent(self.root.winfo_id())
            style = ctypes.windll.user32.GetWindowLongW(hwnd, gwl_exstyle)
            style = (style & ~ws_ex_toolwindow) | ws_ex_appwindow
            ctypes.windll.user32.SetWindowLongW(hwnd, gwl_exstyle, style)
            self.root.withdraw()
            self.root.after(10, self.root.deiconify)
        except Exception as exc:
            print(f"Error restoring taskbar button: {exc}")

    def minimize_window(self):
        """Minimize, which needs the native frame back for the moment it takes.

        iconify() is ignored while overrideredirect is set. The frame goes on,
        the window minimizes with it, and restore_from_taskbar takes it off
        again when the window comes back.
        """
        self._undecorated = False
        self.root.overrideredirect(False)
        self.root.iconify()

    def restore_from_taskbar(self, event=None):
        """Take the native frame off again once the window is back."""
        if self._undecorated or self.root.state() != "normal":
            return
        self._undecorated = True
        self.root.overrideredirect(True)
        self.restore_taskbar_button()

    def start_window_drag(self, event):
        self._drag_offset = (event.x_root - self.root.winfo_x(),
                             event.y_root - self.root.winfo_y())

    def drag_window(self, event):
        offset = getattr(self, "_drag_offset", None)
        if offset is None:
            return
        self.root.geometry("+%d+%d" % (event.x_root - offset[0],
                                       event.y_root - offset[1]))

    def start_window_resize(self, event):
        self._resize_origin = (event.x_root, event.y_root,
                               self.root.winfo_width(), self.root.winfo_height())

    def resize_window(self, event):
        origin = getattr(self, "_resize_origin", None)
        if origin is None:
            return
        x, y, width, height = origin
        floor_w, floor_h = self.root.minsize()
        self.root.geometry("%dx%d" % (
            max(floor_w, width + event.x_root - x),
            max(floor_h, height + event.y_root - y),
        ))

    def icon_path(self):
        """Return the app icon, or None when it is nowhere it is looked for."""
        return find_icon()

    # The title bar's logo. icon.ico stores every size as a PNG and Tk 8.6
    # reads PNG directly, so the entry nearest the size wanted is handed over
    # as it is. Pillow would give CTkImage and HighDPI scaling with it, but it
    # is not currently a dependency and one 20px image is not enough reason to
    # make it one.
    LOGO_SIZE = 24
    # The 8-byte PNG signature, spelled in hex so no escape survives a
    # round trip through a tool that rewrites this file.
    PNG_MAGIC = bytes.fromhex("89504e470d0a1a0a")

    @staticmethod
    def choose_icon_size(widths, size):
        """Which stored icon size to draw at ``size`` px.

        The smallest entry at or above it, falling back to the largest below.
        Tk cannot scale an image up -- subsample only divides -- so an entry
        under the target is drawn small rather than filled, and a tie has to
        break upward: asking 20 of a file holding 16 and 24 was taking the 16.
        """
        at_or_above = [width for width in widths if width >= size]
        return min(at_or_above) if at_or_above else max(widths)

    def logo_image(self, size):
        """The app icon at roughly ``size`` px as a Tk image, or None."""
        try:
            path = self.icon_path()
            if not path:
                return None
            with open(path, "rb") as handle:
                data = handle.read()
            entries = {}
            for index in range(struct.unpack_from("<H", data, 4)[0]):
                width, _h, _c, _r, _p, _b, length, offset = struct.unpack_from(
                    "<BBBBHHII", data, 6 + index * 16)
                blob = data[offset:offset + length]
                if not blob.startswith(self.PNG_MAGIC):
                    continue
                # 0 means 256 in an icon directory, which is the one entry
                # that would otherwise sort as the smallest.
                entries[width or 256] = blob
            if not entries:
                return None
            chosen = self.choose_icon_size(entries, size)
            best = (chosen, entries[chosen])
            image = tkinter.PhotoImage(data=base64.b64encode(best[1]))
            # Tk cannot resample: subsample divides by a whole number and
            # zoom multiplies by one. An oversized entry is stepped down to
            # the nearest whole factor rather than left at full size.
            factor = max(1, round(best[0] / size))
            return image.subsample(factor) if factor > 1 else image
        except Exception as exc:
            print(f"Error loading logo: {exc}")
            return None

    def set_window_icon(self):
        """Set a custom window/taskbar icon instead of the default blue icon."""
        try:
            path = self.icon_path()
            if path:
                self.root.iconbitmap(path)
        except Exception as e:
            print(f"Error setting window icon: {e}")

    def setup_appearance(self):
        self.appearance_mode = self.load_appearance_mode()
        ctk.set_appearance_mode(self.appearance_mode)
        ctk.set_default_color_theme("dark-blue")
        
        # Compact but readable — full Consolas for timing-tool alignment.
        self.GLOBAL_FONT_FAMILY = "Consolas"
        self.GLOBAL_FONT_SIZE = 12
        self.GLOBAL_FONT = (self.GLOBAL_FONT_FAMILY, self.GLOBAL_FONT_SIZE)
        self.COMPACT_FONT_SIZE = 12
        self.COMPACT_FONT = (self.GLOBAL_FONT_FAMILY, self.COMPACT_FONT_SIZE)
        self.COMPACT_BOLD = (self.GLOBAL_FONT_FAMILY, self.COMPACT_FONT_SIZE, "bold")
        self.HEADER_FONT = (self.GLOBAL_FONT_FAMILY, 12, "bold")
        self.TAB_FONT = (self.GLOBAL_FONT_FAMILY, 13, "bold")
        self.ROW_PADX = 4
        self.ROW_PADY = 0
        self.SECTION_GAP = 3
        self.ROW_HEIGHT = 20
        # Content-width timing rows: short name gutter, values pack after labels.
        self.NAME_MINSIZE = 74
        self.VALUE_MINSIZE = 30
        self.VALUE_PADX = 6

        # Each tuple is (light mode, dark mode). CustomTkinter automatically
        # updates every widget using these colors when the mode changes.
        # The dark surfaces sit lower than they did. The steps between them
        # are kept, and the shading step widened by one, because the same
        # 4-value lift reads as less separation the darker the pair gets.
        self.BG_COLOR = ("#F1F5F9", "#161616")
        self.BG_COLOR2 = ("#FFFFFF", "#1C1C1C")
        self.SECTION_COLOR = ("#E2E8F0", "#1C1C1C")
        self.ROW_COLOR = ("#F8FAFC", "#202020")
        self.BORDER_COLOR = ("#CBD5E1", "#101010")
        self.TEXT_COLOR = ("#0F172A", "#FFFFFF")
        self.VALUE_COLOR = ("#B91C1C", "#FF4D4D")
        self.HIGHLIGHT_COLOR = ("#E8EEF5", "#1D1D1D")
        # The tab strip follows the title bar and the footer link: the app's
        # red rather than the theme's blue. Saturated in light, muted in dark,
        # which is the pair the blue used and the reason a flat #B91C1C in
        # both looked like a warning banner against the dark surfaces.
        self.TAB_SELECTED_COLOR = ("#B91C1C", "#5D1A1A")
        self.TAB_UNSELECTED_COLOR = ("#D7E1EC", "#2E2E2E")
        self.TAB_HOVER_COLOR = ("#DC2626", "#792A2A")
        # White on the selected tab in both modes. TEXT_COLOR is near-black
        # in light mode, and near-black on a dark red is unreadable -- the
        # blue it replaced was light enough to carry it.
        self.TAB_SELECTED_TEXT_COLOR = ("#FFFFFF", "#FFFFFF")
        self.TAB_UNSELECTED_HOVER_COLOR = ("#C5D2E0", "#3A3A3A")
        self.HEADER_COLOR = ("#E2E8F0", "#222222")
        self.SUBTITLE_COLOR = ("#475569", "#B0B0B0")
        # A rule between Summary blocks: visible against both backgrounds
        # without competing with the values, which are the loudest thing on
        # the tab and should stay that way.
        self.RULE_COLOR = ("#94A3B8", "#4A4A4A")
        # The app's own red, used for the title-bar name and the footer link.
        # Same pair as VALUE_COLOR, named separately because these two follow
        # the brand rather than the reading-is-red rule the tables use.
        self.BRAND_COLOR = ("#B91C1C", "#FF4D4D")
        self.BRAND_HOVER_COLOR = ("#DC2626", "#FF8080")
        self.root.configure(fg_color=self.BG_COLOR)

    def settings_path(self):
        """Return a per-user settings path that also works for packaged builds."""
        base = os.environ.get("LOCALAPPDATA") or os.path.expanduser("~")
        return os.path.join(base, "RochViewer", "settings.json")

    def load_settings(self):
        """Return the saved settings, or an empty dict when there are none."""
        try:
            with open(self.settings_path(), "r", encoding="utf-8") as settings_file:
                saved = json.load(settings_file)
                return saved if isinstance(saved, dict) else {}
        except (OSError, ValueError, TypeError, AttributeError):
            return {}

    def save_setting(self, key, value):
        """Merge one setting into the file, leaving the others alone."""
        settings = self.load_settings()
        settings[key] = value
        try:
            path = self.settings_path()
            os.makedirs(os.path.dirname(path), exist_ok=True)
            with open(path, "w", encoding="utf-8") as settings_file:
                json.dump(settings, settings_file)
        except OSError as e:
            print(f"Could not save setting {key}: {e}")

    def load_appearance_mode(self):
        mode = self.load_settings().get("appearance_mode", "Dark")
        if str(mode).lower() in ("light", "dark"):
            return str(mode).title()
        return "Dark"

    # The tools sit on the tab strip's own row, right-aligned, rather than in
    # a band of their own above it. They were the only thing in that band, so
    # it cost a row of height to hold three buttons against an empty left
    # half -- and left them floating clear of the tabs they sit beside.
    #
    # Placed on the tabview rather than packed into it: CTkTabview owns its
    # internal layout and packing into it fights that, while place() only
    # borrows the corner. Vertically centred in the strip by construction.
    TAB_STRIP_HEIGHT = 36
    TOOL_BUTTON_HEIGHT = 20
    TOOL_BUTTON_WIDTH = 60

    def build_tab_strip_tools(self):
        bar = ctk.CTkFrame(self.tabview, corner_radius=0, fg_color="transparent")
        bar.place(relx=1.0, y=(self.TAB_STRIP_HEIGHT - self.TOOL_BUTTON_HEIGHT) // 2,
                  x=-8, anchor="ne")
        self.appearance_toolbar = bar

        def tool(text, command):
            return ctk.CTkButton(
                bar, text=text, command=command,
                width=self.TOOL_BUTTON_WIDTH, height=self.TOOL_BUTTON_HEIGHT,
                fg_color=self.TAB_UNSELECTED_COLOR,
                hover_color=self.TAB_UNSELECTED_HOVER_COLOR,
                text_color=self.TEXT_COLOR, font=self.COMPACT_BOLD,
            )

        # One button rather than a Light/Dark pair. The pair spent its width
        # showing the mode you are not in; this shows the mode you are in and
        # switches when pressed, which is the only thing the other half did.
        self.appearance_button = tool(self.appearance_mode,
                                      self.toggle_appearance_mode)
        self.appearance_button.pack(side="right", padx=(4, 0))

        # Every row from the reading tabs in one searchable list. The tabs are
        # laid out for reading a set at a glance, which is the wrong shape
        # when you already know the name of the field you want.
        self.advanced_button = tool("Advanced", self.open_advanced)
        self.advanced_button.pack(side="right", padx=(4, 0))

        # Everything that moves, in its own window: it polls and keeps
        # min/max/average, which does not belong on a tab that reads settings.
        self.telemetry_button = tool("Telemetry", self.open_dimm_telemetry)
        self.telemetry_button.pack(side="right", padx=(4, 0))

        # TAB_STRIP_HEIGHT is what the tabview was asked for, not what the
        # strip inside it draws as -- it came out 26 here, so centring on 36
        # left the buttons 5px high of the tab text. Measured against the
        # drawn strip instead, which also survives a theme or font change.
        #
        # On the window's <Configure> rather than after_idle: idle runs before
        # the window is laid out, where the strip and the tabview share a
        # rooty and the offset comes out zero. Configure fires once there is a
        # size to measure and again whenever it changes, so the buttons stay
        # centred through a resize as well.
        #
        # Bound on the window because a CustomTkinter widget's bind() raises
        # NotImplementedError -- it does not forward to the frame it draws in.
        self.root.bind("<Configure>", self.align_tab_strip_tools, add="+")

    def align_tab_strip_tools(self, _event=None):
        """Centre the tool buttons on the drawn tab strip."""
        try:
            strip = self.tabview._segmented_button
            offset = strip.winfo_rooty() - self.tabview.winfo_rooty()
            middle = offset + (strip.winfo_height()
                               - self.appearance_toolbar.winfo_height()) // 2
            if middle > 0 and self.appearance_toolbar.place_info().get(
                    "y") != str(middle):
                self.appearance_toolbar.place_configure(y=middle)
        except Exception:
            # Left where it was placed: a few pixels off beats no buttons.
            pass

    def toggle_appearance_mode(self):
        """Swap Light for Dark, and say which one is now on."""
        self.change_appearance_mode(
            "Light" if self.appearance_mode == "Dark" else "Dark")
        self.appearance_button.configure(text=self.appearance_mode)

    def change_appearance_mode(self, mode):
        """Apply and remember the selected Light or Dark appearance."""
        self.appearance_mode = str(mode).title()
        ctk.set_appearance_mode(self.appearance_mode)
        self.save_setting("appearance_mode", self.appearance_mode)

    def telemetry_modules(self):
        """Module identity for the telemetry window, keyed by channel letter.

        SMBIOS names the module and the slot; the SPD adds the module vendor,
        which firmware often leaves as "Unknown" -- it reads G.Skill off the
        DIMM here while WMI has nothing.
        """
        from rochviewer.memory.dimm_inventory import rank_numeric, read_modules

        modules = []
        try:
            spd_by_part = {}
            try:
                from rochviewer.memory.ddr5_spd import read_identity

                for entry in read_identity() or []:
                    part = (entry.get("part_number") or "").strip()
                    if part:
                        spd_by_part[part] = entry
            except Exception:
                spd_by_part = {}

            for module in read_modules():
                enriched = dict(module)
                enriched["rank_numeric"] = rank_numeric(module.get("rank_count", 0))
                spd = spd_by_part.get((module.get("part_number") or "").strip())
                if spd and spd.get("module_manufacturer"):
                    enriched["spd_vendor"] = spd["module_manufacturer"]
                modules.append(enriched)
        except Exception:
            return []
        return modules

    def sensor_groups(self):
        """Return the live sensor rows grouped by section, for the window.

        These are the rows that used to be the Sensors tab. They stay in
        TIMINGS, but a tab shows one instant, and what a rail does over time
        is the interesting part.

        Groups come out in SENSOR_GROUP_ORDER, which reads outward from what
        the silicon is doing to what is feeding it. A group the platform
        declares but that order does not name keeps its declaration order at
        the end, so another platform's sections are never dropped.

        Each reading is ``(label, read, parent)``. A parent names the row this
        one folds under -- the per-processor effective clocks under their
        aggregate -- and is None for a row that stands on its own.
        """
        groups = []
        for timing in TIMINGS:
            if timing.get("Tab") not in WINDOWED_TABS:
                continue
            title = timing.get("Category") or "Sensors"
            existing = next((rows for name, rows in groups if name == title), None)
            if existing is None:
                existing = []
                groups.append((title, existing))
            existing.append(
                (timing.get("name", ""),
                 lambda item=timing: self._read_compact_value(item),
                 timing.get("Parent"))
            )

        def position(entry):
            title = entry[0]
            if title in SENSOR_GROUP_ORDER:
                return SENSOR_GROUP_ORDER.index(title)
            return len(SENSOR_GROUP_ORDER)

        return sorted(groups, key=position)

    # The reading tabs, in the order the tab strip shows them. Summary is left
    # out because every row on it is repeated from one of these, and the
    # telemetry window is its own thing with its own statistics.
    ADVANCED_TABS = ("System Info", "Timings", "Skew", "Misc")

    def advanced_entries(self):
        """[(tab, category, name, read())] for everything the tabs display.

        A row that reads two channels returns a (A, B) pair, so the window can
        column them under A1/B1 headings the way the tabs do. Squeezed into
        one string it read "A 2  |  B 1", which is the only shape tWPRE --
        the one timing whose channels differ -- ever appeared in.
        """
        entries = []
        for tab in self.ADVANCED_TABS:
            for timing in TIMINGS:
                if timing.get("Tab") != tab or timing.get("diagnostic"):
                    continue
                name = timing.get("name")
                # The tables use blank rows as spacers; they are layout, not
                # readings, and a searchable list has nothing to do with them.
                if not name or not name.strip():
                    continue
                if is_dual_timing(timing):
                    read = (lambda timing=timing: (
                        self._read_compact_side(timing, "a"),
                        self._read_compact_side(timing, "b"),
                    ))
                else:
                    read = lambda timing=timing: self._read_compact_value(timing)
                entries.append((
                    tab, timing.get("Category") or "", name, read,
                ))
        return entries

    def open_advanced(self):
        """Open the searchable all-rows window, or focus the open one."""
        existing = getattr(self, "_advanced_window", None)
        if existing is not None and existing.winfo_exists():
            existing.lift()
            existing.focus()
            return
        try:
            from rochviewer.ui.advanced_window import AdvancedWindow
        except Exception as exc:
            print(f"Advanced window unavailable: {exc}")
            return

        theme = {
            "bg": self.BG_COLOR,
            "header_bg": self.BG_COLOR2,
            "band": self.HIGHLIGHT_COLOR,
            "text": self.TEXT_COLOR,
            "muted": self.SUBTITLE_COLOR,
            "value": self.VALUE_COLOR,
            "button": self.TAB_UNSELECTED_COLOR,
            "button_hover": self.TAB_UNSELECTED_HOVER_COLOR,
            "font": self.COMPACT_FONT,
            "bold": self.COMPACT_BOLD,
        }
        self._advanced_window = AdvancedWindow(
            self.root,
            theme,
            self.advanced_entries(),
            channel_labels=self._channel_headers(None, "A1", "B1"),
            on_close=lambda: setattr(self, "_advanced_window", None),
            icon_path=self.icon_path(),
        )

    def open_dimm_telemetry(self):
        """Open the sensor telemetry window, or focus the open one."""
        existing = getattr(self, "_telemetry_window", None)
        if existing is not None and existing.winfo_exists():
            existing.lift()
            existing.focus()
            return
        try:
            from rochviewer.memory.ddr5_telemetry import read_dimm_telemetry
            from rochviewer.ui.dimm_telemetry_window import DimmTelemetryWindow
        except Exception as exc:
            print(f"DIMM telemetry unavailable: {exc}")
            return

        theme = {
            "bg": self.BG_COLOR,
            "header_bg": self.BG_COLOR2,
            "band": self.HIGHLIGHT_COLOR,
            "text": self.TEXT_COLOR,
            "muted": self.SUBTITLE_COLOR,
            "value": self.VALUE_COLOR,
            "button": self.TAB_UNSELECTED_COLOR,
            "button_hover": self.TAB_UNSELECTED_HOVER_COLOR,
            "font": self.COMPACT_FONT,
            "bold": self.COMPACT_BOLD,
        }
        self._telemetry_window = DimmTelemetryWindow(
            self.root,
            theme,
            read_dimm_telemetry,
            self.telemetry_modules,
            on_close=lambda: setattr(self, "_telemetry_window", None),
            auto_open=bool(self.load_settings().get("telemetry_auto_open")),
            on_auto_open=lambda value: self.save_setting(
                "telemetry_auto_open", value
            ),
            sensor_groups=self.sensor_groups(),
            icon_path=self.icon_path(),
        )

    def get_memory_part_numbers(self):
        """Return the installed modules per channel, named by the board's own
        slot labels.

        Slots come from SMBIOS DeviceLocator rather than the position of the
        record in the query. Position is a handle order, not a slot order: on
        the MSI Z790-P the two modules arrive as records 1 and 3 while the board
        calls them Controller0-DIMMA2 and Controller1-DIMMB2, so the table this
        used to consult reported them as B1 and A1 - the wrong slot numbers and,
        worse, the channels the wrong way round.
        """
        try:
            # Same decode the System Info rank/die/size rows read, so the two
            # displays cannot disagree about the installed modules.
            memory_info = read_modules()

            channels = {}
            for info in memory_info:
                slot = info.get("slot")
                if not slot:
                    # The board did not name the socket; listing the module
                    # under a guessed one is how this went wrong before.
                    continue
                # (slot, description): the strip renders the slot brighter
                # than the rest, so they stay separate rather than one string.
                channels.setdefault(channel_of(slot), []).append((
                    slot,
                    "%s (%s, %s, %s)" % (
                        info["part_number"], info["capacity"],
                        info["rank"], info["ic"],
                    ),
                ))

            for entries in channels.values():
                entries.sort()
            return channels.get("A", []), channels.get("B", [])

        except Exception as e:
            print(f"Error retrieving memory info: {e}")
            return [], []

    def create_widgets(self):
        channel_a, channel_b = self.get_memory_part_numbers()
        self.main_frame = ctk.CTkFrame(self.root, corner_radius=6, fg_color=self.BG_COLOR)
        self.main_frame.pack(fill="both", expand=True, padx=5, pady=(2, 4))

        self.build_footer()

        self.tabview = ctk.CTkTabview(
            self.main_frame,
            fg_color=self.BG_COLOR,
            segmented_button_fg_color=self.BG_COLOR2,
            segmented_button_selected_color=self.TAB_SELECTED_COLOR,
            segmented_button_selected_hover_color=self.TAB_HOVER_COLOR,
            segmented_button_unselected_color=self.TAB_UNSELECTED_COLOR,
            segmented_button_unselected_hover_color=self.TAB_UNSELECTED_HOVER_COLOR,
            corner_radius=8,
            border_width=0,
            height=36,
            anchor="w",
        )
        self.tabview._segmented_button.configure(
            corner_radius=8,
            border_width=1,
            fg_color=self.BG_COLOR,
            selected_color=self.TAB_SELECTED_COLOR,
            selected_hover_color=self.TAB_HOVER_COLOR,
            unselected_color=self.TAB_UNSELECTED_COLOR,
            unselected_hover_color=self.TAB_UNSELECTED_HOVER_COLOR,
            text_color=self.TEXT_COLOR,
            font=self.TAB_FONT,
        )
        self.tabview._segmented_button._text_color = self.TEXT_COLOR
        self.tabview._segmented_button._selected_text_color = (
            self.TAB_SELECTED_TEXT_COLOR
        )
        self.tabview.pack(fill="both", expand=True, padx=2, pady=(2, 2))
        self.build_tab_strip_tools()
        self.tab_names = select_tab_names(TIMINGS)
        for name in self.tab_names:
            self.tabview.add(name)
        self.tab_frames = {}
        self.grid_frames = {}
        for name in self.tab_names:
            # Summary is sized to hold everything it shows, so it is drawn in
            # a plain frame: a scrollable one keeps a scrollbar gutter down
            # the right whether or not it is scrolling, and the tab meant to
            # be read at a glance was giving up width to a control it never
            # used. The reading tabs are longer than any window and keep
            # theirs.
            holder = (ctk.CTkFrame if name in self.UNSCROLLED_TABS
                      else ctk.CTkScrollableFrame)(
                self.tabview.tab(name),
                corner_radius=0,
                fg_color=self.BG_COLOR
            )
            holder.pack(fill="both", expand=True)
            self.tab_frames[name] = holder

            frame = ctk.CTkFrame(holder, corner_radius=0, fg_color=self.BG_COLOR)
            frame.pack(fill="both", expand=True, padx=2, pady=1)

            if name == "Summary":
                # Full-width system strip + top-aligned columns (no empty stretch band).
                column_count = self.summary_columns
                # Each column takes the width its own rows need; only the last
                # one stretches. Equal thirds looked tidy in the abstract and
                # read badly in practice: the middle column needs 126px and was
                # handed 256, so its values sat a hundred pixels clear of the
                # next column's labels.
                for column in range(column_count):
                    frame.grid_columnconfigure(
                        column, weight=1 if column == column_count - 1 else 0
                    )
                frame.grid_rowconfigure(0, weight=0)
                frame.grid_rowconfigure(1, weight=0)

                full_width_frame = ctk.CTkFrame(
                    frame, corner_radius=0, fg_color=self.BG_COLOR
                )
                full_width_frame.grid(
                    row=0, column=0, columnspan=column_count, sticky="ew",
                    pady=(0, self.SECTION_GAP)
                )
                full_width_frame.grid_columnconfigure(0, weight=1)

                compact_columns = []
                for column in range(column_count):
                    column_frame = ctk.CTkFrame(
                        frame, corner_radius=0, fg_color=self.BG_COLOR
                    )
                    # Equal-width columns, top-aligned content, full panel width.
                    # No gutter: the row shading has to carry across the
                    # whole tab, and a gap between columns would break every
                    # band into three.
                    column_frame.grid(row=1, column=column, sticky="nsew")
                    column_frame.grid_columnconfigure(0, weight=1)
                    compact_columns.append(column_frame)
                self.grid_frames[name] = {
                    "FullWidth": full_width_frame,
                    "Columns": compact_columns,
                }
                continue

            if name == "System Info":
                # General information gets its own full-width tab instead of
                # sharing space with the timing sections.
                frame.grid_columnconfigure(0, weight=1)
                frame.grid_rowconfigure(0, weight=1)
                info_frame = ctk.CTkFrame(
                    frame, corner_radius=0, fg_color=self.BG_COLOR
                )
                info_frame.grid(row=0, column=0, sticky="nsew")
                info_frame.grid_columnconfigure(0, weight=1)

                # The loader expects both keys, but System Info has no right
                # column. Keep an ungridded placeholder for compatibility.
                right_placeholder = ctk.CTkFrame(
                    frame, corner_radius=0, fg_color=self.BG_COLOR
                )
                self.grid_frames[name] = {
                    "Left": info_frame,
                    "Right": right_placeholder,
                }
                continue

            # Not uniform on a shaded tab: each half is sized to its own
            # content by _stretch_tab_halves, with COLUMN_GAP between
            # them. Held equal, the narrower half carried a void the
            # width of the difference before the other one started.
            uniform = None if name in SHADED_TABS else "equal"
            frame.grid_columnconfigure(0, weight=1, uniform=uniform)
            frame.grid_columnconfigure(1, weight=1, uniform=uniform)
            # The halves of a banded tab touch, so a row's shading runs across
            # the tab in one piece. Everywhere else they keep their gutter.
            gutter = 0 if name in SHADED_TABS else 3
            left_frame = ctk.CTkFrame(frame, corner_radius=0, fg_color=self.BG_COLOR)
            left_frame.grid(row=0, column=0, sticky="nsew", padx=(0, gutter))
            left_frame.grid_columnconfigure(0, weight=1)
            right_frame = ctk.CTkFrame(frame, corner_radius=0, fg_color=self.BG_COLOR)
            right_frame.grid(row=0, column=1, sticky="nsew", padx=(gutter, 0))
            right_frame.grid_columnconfigure(0, weight=1)
            self.grid_frames[name] = {"Left": left_frame, "Right": right_frame}
        # Keep one DIMM strip at the bottom of the window.
        # The installed modules are their own block, so a rule separates them
        # from the tab above the way the Summary separates its own blocks.
        self.bottom_rule = ctk.CTkFrame(
            self.root, corner_radius=0, height=2, fg_color=self.RULE_COLOR
        )
        self.bottom_rule.pack(fill="x", padx=9, pady=(2, 0))

        self.bottom_part_number_frame = ctk.CTkFrame(
            self.root,
            corner_radius=0,
            fg_color=self.BG_COLOR,
            border_width=0,
        )
        self.bottom_part_number_frame.pack(fill="x", padx=5, pady=(1, 4))
        self.bottom_part_number_inner_frame = ctk.CTkFrame(
            self.bottom_part_number_frame,
            corner_radius=0,
            fg_color="transparent",
            border_width=0,
        )
        self.bottom_part_number_inner_frame.pack(fill="x", padx=2, pady=1)
        self.bottom_part_number_inner_frame.grid_columnconfigure(0, weight=1, uniform="equal")
        self.bottom_part_number_inner_frame.grid_columnconfigure(1, weight=1, uniform="equal")

        self._dimm_strip_column(channel_a, 0)
        self._dimm_strip_column(channel_b, 1)

    def _dimm_strip_column(self, modules, column):
        """One channel of the bottom strip: slot in front, details behind it.

        The slot is what a reader is looking for -- which stick this is -- so
        it carries the text colour while the description stays subdued.
        """
        holder = ctk.CTkFrame(
            self.bottom_part_number_inner_frame, corner_radius=0,
            fg_color="transparent",
        )
        holder.grid(row=0, column=column, sticky="nsew")
        for slot, detail in modules:
            line = ctk.CTkFrame(holder, corner_radius=0, fg_color="transparent")
            line.pack()
            ctk.CTkLabel(
                line, text=slot, font=self.COMPACT_BOLD,
                height=self.ROW_HEIGHT, anchor="e", padx=4, pady=1,
                text_color=self.TEXT_COLOR, fg_color="transparent",
            ).pack(side="left")
            ctk.CTkLabel(
                line, text=detail, font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT, anchor="w", padx=2, pady=1,
                text_color=self.SUBTITLE_COLOR, fg_color="transparent",
            ).pack(side="left")

    # --- Footer.
    #
    # Packed before the tabview so it claims the bottom of the frame and the
    # tabs take what is left; packed after, the expanding tabview would have
    # already taken the height and pushed this off the window.
    #
    # It also carries the resize grip, which the window has no border for now
    # that it draws its own frame.
    TWITTER_URL = "https://x.com/MateoPCTech"
    TWITTER_HANDLE = "@MateoPCTech"
    FOOTER_HEIGHT = 24
    GRIP_SIZE = 14

    def build_footer(self):
        footer = ctk.CTkFrame(self.main_frame, height=self.FOOTER_HEIGHT,
                              corner_radius=0, fg_color="transparent")
        footer.pack(fill="x", side="bottom", padx=4, pady=(0, 1))
        footer.pack_propagate(False)
        self.footer = footer

        link = ctk.CTkLabel(
            footer, text=self.TWITTER_HANDLE, font=self.COMPACT_BOLD,
            text_color=self.BRAND_COLOR, cursor="hand2",
        )
        link.pack(side="left", padx=4)
        link.bind("<Button-1>", self.open_twitter)
        # Colour is the whole affordance here -- there is no button edge to
        # tell you it is clickable, so it lifts a shade under the pointer.
        link.bind("<Enter>",
                  lambda _e: link.configure(text_color=self.BRAND_HOVER_COLOR))
        link.bind("<Leave>",
                  lambda _e: link.configure(text_color=self.BRAND_COLOR))
        self.twitter_link = link

        grip = ctk.CTkLabel(footer, text="◢", width=self.GRIP_SIZE,
                            font=self.COMPACT_BOLD,
                            text_color=self.SUBTITLE_COLOR, cursor="sizing")
        grip.pack(side="right", padx=2)
        grip.bind("<Button-1>", self.start_window_resize)
        grip.bind("<B1-Motion>", self.resize_window)

    def open_twitter(self, event=None):
        try:
            webbrowser.open_new_tab(self.TWITTER_URL)
        except Exception as exc:
            print(f"Error opening {self.TWITTER_URL}: {exc}")

    def setup_window_geometry(self):
        screen_width = self.root.winfo_screenwidth()
        screen_height = self.root.winfo_screenheight()

        # The asked-for startup size, measured against this bench: at 790 wide
        # nothing on any of the five tabs clips. A board that needs more
        # Summary columns than the base layout still gets them -- the width
        # grows by a column each time rather than squeezing, and
        # _widen_to_fit_tabs is still there for a tab that needs more than
        # either.
        extra_columns = max(0, self.summary_columns - SUMMARY_BASE_COLUMNS)
        column_width = 190
        target_width = self.WINDOW_WIDTH + extra_columns * column_width
        target_height = self.WINDOW_HEIGHT
        min_width = target_width
        window_width = min(target_width, max(min_width, screen_width - 80))
        window_height = min(target_height,
                            max(self.MIN_WINDOW_HEIGHT, screen_height - 120))
        x = max(0, (screen_width - window_width) // 2)
        y = max(0, (screen_height - window_height) // 2)

        self.root.geometry(f"{window_width}x{window_height}+{x}+{y}")
        self.root.minsize(min_width, self.MIN_WINDOW_HEIGHT)
        # Both kept because the fitted-width pass runs before the window has
        # been mapped, where winfo_width() and winfo_height() answer with Tk's
        # 200x200 default rather than with what was just asked for. Comparing
        # the tabs' requirement against that default made every tab look too
        # wide for the window and grew it every time.
        self._window_width = window_width
        self._window_height = window_height

    # Tabs drawn without a scrollbar, because the window is sized to fit
    # them whole. Anything taller than the window would be cut off unseen
    # here rather than reachable, so a tab only belongs on this list while
    # its content clears the viewport -- see the fit check in the tests.
    UNSCROLLED_TABS = ("Summary",)

    # The startup size. Pinned rather than derived: the tabs were measured at
    # this width and nothing on any of the five clips.
    WINDOW_WIDTH = 710
    WINDOW_HEIGHT = 790
    MIN_WINDOW_HEIGHT = 654

    # Chrome around the two halves of a tab: the notebook border, the padding
    # either side, and the vertical scrollbar the long tabs carry.
    TAB_CHROME_WIDTH = 46

    def required_tab_width(self):
        """The width the widest tab needs to show its columns whole.

        Measured rather than fixed. The startup size is fitted to the Summary,
        which is the narrowest tab; Timings and Misc are wider, and grew wider
        again as rows were added, so a constant tuned against one tab clipped
        the others -- the right-hand channel of a dual row was cut off mid
        value, and a decoded string like "Timer Stops at 2048th clocks" simply
        ended.
        """
        widest = 0
        for frames in self.grid_frames.values():
            # Most tabs hold {side: frame}; some hold a plain list of frames.
            halves = frames.values() if hasattr(frames, "values") else frames
            needed = 0
            for frame in halves:
                if frame is None or not hasattr(frame, "winfo_reqwidth"):
                    continue
                # System Info keeps an ungridded right-hand placeholder
                # for callers that expect both keys. Counted, it added
                # its width to a tab that does not draw it and the
                # window came out wider than any tab's content.
                if not frame.winfo_manager():
                    continue
                try:
                    needed += frame.winfo_reqwidth()
                except Exception:
                    continue
            widest = max(widest, needed)
        return widest + self.TAB_CHROME_WIDTH if widest else 0

    def _widen_to_fit_tabs(self):
        """Grow the window if a tab needs more width than the startup size."""
        try:
            self.root.update_idletasks()
            needed = self.required_tab_width()
            if not needed:
                return
            current = getattr(self, "_window_width", self.root.winfo_width())
            screen = self.root.winfo_screenwidth()
            # required_tab_width() adds TAB_CHROME_WIDTH to every tab, and not
            # every tab spends all of it -- a tab that does not scroll pays no
            # scrollbar. Measured here: it asks 822 where 790 clips nothing,
            # 32px inside that allowance. So the allowance is slack, and the
            # window only grows for a tab that needs more than the pinned
            # width even after it is discounted.
            if needed - self.TAB_CHROME_WIDTH <= current:
                return
            target = min(needed, screen - 80)
            if target <= current:
                return
            height = getattr(self, "_window_height", self.root.winfo_height())
            x = max(0, (screen - target) // 2)
            self.root.geometry(f"{target}x{height}+{x}+{self.root.winfo_y()}")
            self.root.minsize(min(target, screen - 80),
                              self.MIN_WINDOW_HEIGHT)
            self._window_width = target
        except Exception:
            # A tab that cannot be measured keeps the startup width; a clipped
            # column is better than no window.
            pass

    def _stretch_tab_halves(self):
        """Give every tab the same content width, so a band crosses it whole.

        Each tab page sizes itself to its own content, so a narrower tab left
        the rest of the window bare: Skew's two halves came to 602px inside an
        870px window, and its row shading stopped there rather than running
        across. Holding both halves of every tab to the widest tab's own half
        makes the bands the same width on all of them.

        A minimum rather than a fixed size, so a tab wider than this one keeps
        its width instead of being squeezed.
        """
        try:
            self.root.update_idletasks()
            # Measured from the tabs themselves rather than derived from
            # the window size by subtracting a chrome estimate, which left
            # the widest tab a few pixels past every other one.
            widest = 0
            for frames in self.grid_frames.values():
                if not hasattr(frames, 'values'):
                    continue
                # Not winfo_ismapped: only the tab on screen is mapped,
                # so measuring that way sized every other tab to zero.
                total = sum(
                    frame.winfo_reqwidth()
                    for frame in frames.values()
                    if hasattr(frame, 'winfo_manager')
                    and frame.winfo_manager()
                )
                # Two halves means the gap between them counts too.
                if len([f for f in frames.values()
                        if hasattr(f, 'winfo_manager')
                        and f.winfo_manager()]) > 1:
                    total += self.COLUMN_GAP
                widest = max(widest, total)
            if widest <= 0:
                return
            for name, frames in self.grid_frames.items():
                if name not in SHADED_TABS or not hasattr(frames, 'get'):
                    continue
                left = frames.get('Left')
                right = frames.get('Right')
                if left is None:
                    continue
                parent = left.master
                # System Info is one full-width column; the timing tabs
                # are two halves, each as wide as it needs to be with
                # the gap between them.
                if half_is_used(right):
                    # The left half is its own content plus the gap; the
                    # right takes whatever is left, so a narrow tab still
                    # fills the window and its bands reach the edge
                    # rather than stopping where its content does.
                    # Rounded to an even width, for the reason
                    # summary_column_width spells out: the draw engine rounds
                    # a fill down to an even number of pixels, so an odd
                    # column leaves its own last pixel unpainted. The halves
                    # tile with no gutter, so that bare pixel read as a
                    # hairline down the seam, cutting every band in two --
                    # 285px on Timings and 355px on Skew, both odd.
                    lead = summary_column_width(
                        left.winfo_reqwidth() + self.COLUMN_GAP, is_last=False
                    )
                    parent.grid_columnconfigure(0, minsize=lead)
                    parent.grid_columnconfigure(
                        1, minsize=max(right.winfo_reqwidth(), widest - lead))
                else:
                    parent.grid_columnconfigure(0, minsize=widest)
                    # An empty half still asks for CustomTkinter's default
                    # 200px frame, and the grid hands it a slice of the tab
                    # for holding nothing -- 56px on Misc, which the row
                    # shading then stopped short of. Taken out of the grid
                    # rather than left at zero width, which is what System
                    # Info's unused half has always done; grid_remove keeps
                    # its configuration, so it comes back with its sections
                    # if the tab ever splits again.
                    parent.grid_columnconfigure(1, minsize=0, weight=0)
                    if right is not None and right.winfo_manager():
                        right.grid_remove()
        except Exception:
            # A tab that cannot be measured keeps its own width; a short
            # band is better than no window.
            pass

    def _read_compact_side(self, timing, side):
        """Read one side of a dual-channel timing row for the compact view."""
        value_key = f"value_{side}"
        read_type_key = f"read_type_{side}"
        dynamic_key = f"dynamic_params_{side}"
        address_key = f"address_{side}"
        parameters_key = f"parameters_{side}"

        if value_key in timing:
            return resolve_display_value(timing[value_key])
        if timing.get(read_type_key) == "dynamic" and dynamic_key in timing:
            raw_value = read_timing(
                read_type="dynamic", dynamic_params=timing[dynamic_key]
            )
            return apply_formula(raw_value, timing.get("Formula"))
        if timing.get(address_key) is not None and parameters_key in timing:
            raw_value = read_timing(
                address=timing[address_key],
                bit_start=timing[parameters_key]["bit_start"],
                bit_length=timing[parameters_key]["bit_length"],
                read_type="standard",
            )
            return apply_formula(raw_value, timing.get("Formula"))
        if timing.get("value") is not None:
            return resolve_display_value(timing["value"])
        return "N/A"

    def _value_color(self, timing):
        """Dim a value the hardware is not currently acting on.

        Some rows stay readable but no longer apply — tRFC outside Normal
        refresh mode, for instance, where the controller refreshes on tRFC2.
        Showing them at full weight implies they are in effect.
        """
        dim = isinstance(timing, dict) and timing.get("dim")
        try:
            if dim and dim():
                return self.SUBTITLE_COLOR
        except Exception:
            pass
        return self.VALUE_COLOR

    def _register_live_value(self, timing, label):
        """Track a value label so rows marked ``live`` can be re-read.

        Timings are fixed until a reboot; voltages, currents and power move
        constantly, so a value read once at startup is stale the moment it is
        drawn. The row declares which it is, so liveness does not depend on
        which tab happens to render it.

        A row that does not move still has to be read once, and that one read
        can lose a race: UCLK:MCLK needs the clock block and MCLK to both come
        back in the same instant, so a poll running against the same mailbox
        was enough to blank it -- permanently, since nothing read it again.
        Those are collected for one retry rather than made live, because they
        genuinely do not change; the read just has to land once.
        """
        if not isinstance(timing, dict):
            return
        if timing.get("live"):
            self.live_value_labels.append((timing, label))
        elif self._is_blank(label):
            self._blank_labels.append((timing, label))

    @staticmethod
    def _is_blank(label):
        try:
            return str(label.cget("text")).strip() == EM_DASH
        except Exception:
            return False

    def _retry_blank_values(self):
        """Read once more anything whose first read came back empty.

        Runs off the UI thread for the same reason the live refresh does:
        these reads take privileged mutexes. A row that reads blank again is
        dropped rather than retried forever -- at that point the value really
        is unavailable, and an em dash is the honest answer.
        """
        pending, self._blank_labels = self._blank_labels, []
        if not pending:
            return

        def work():
            results = []
            for timing, label in pending:
                try:
                    text = self._read_compact_value(timing)
                except Exception:
                    continue
                if text and str(text).strip() != EM_DASH:
                    results.append((label, text))
            try:
                self.root.after(0, self._apply_values, results)
            except Exception:
                pass

        threading.Thread(target=work, daemon=True).start()

    def start_live_refresh(self):
        """Begin periodically re-reading the live rows."""
        if self.live_value_labels:
            self.root.after(LIVE_REFRESH_MS, self._refresh_live_values)

    def _refresh_live_values(self):
        """Kick off a refresh off the UI thread.

        These reads take privileged mutexes and can block for milliseconds at
        a time, so doing them inline froze the window. The hardware access
        happens on a worker; only the label updates come back to Tk, which is
        not thread-safe.
        """
        if self._live_refresh_busy:
            # Still reading; skip this tick rather than queueing up work.
            self.root.after(LIVE_REFRESH_MS, self._refresh_live_values)
            return
        self._live_refresh_busy = True
        threading.Thread(target=self._read_live_values, daemon=True).start()

    def _read_live_values(self):
        """Worker: read every live row, then hand the results back to Tk."""
        # Any value backed by WMI needs COM initialised on this thread. The
        # CPU name is cached on the main thread so it should not be reached
        # from here, but a row that silently blanks is hard to spot, so
        # initialise anyway rather than rely on that.
        com_ready = False
        try:
            import pythoncom
            pythoncom.CoInitialize()
            com_ready = True
        except Exception:
            pass
        try:
            self._collect_live_values()
        finally:
            if com_ready:
                try:
                    pythoncom.CoUninitialize()
                except Exception:
                    pass

    def _collect_live_values(self):
        results = []
        for timing, label in list(self.live_value_labels):
            try:
                results.append((label, self._read_compact_value(timing)))
            except Exception:
                continue
        try:
            self.root.after(0, self._apply_live_values, results)
        except Exception:
            # The window is gone; nothing left to update.
            self._live_refresh_busy = False

    def _apply_values(self, results):
        """Write read values back to their labels. UI thread only."""
        for label, text in results:
            try:
                if label.winfo_exists() and label.cget("text") != text:
                    label.configure(text=text)
            except Exception:
                # The widget went away (window closing); drop it next pass.
                continue

    def _apply_live_values(self, results):
        self._apply_values(results)
        # Only the live chain books the next tick. The one-shot retry shares
        # the writing but not the scheduling, or it would start a second
        # refresh loop running against the same mailboxes.
        self._live_refresh_busy = False
        self.root.after(LIVE_REFRESH_MS, self._refresh_live_values)

    def _read_compact_value(self, timing):
        """Return the same live value used by the normal tables, in compact form."""
        is_dual = is_dual_timing(timing)
        if is_dual:
            value_a = self._read_compact_side(timing, "a")
            value_b = self._read_compact_side(timing, "b")
            if value_a == value_b:
                return value_a
            return f"A {value_a}  |  B {value_b}"

        if timing.get("read_type") == "dynamic" and "dynamic_params" in timing:
            raw_value = read_timing(
                read_type="dynamic", dynamic_params=timing["dynamic_params"]
            )
            if raw_value is None:
                return "N/A"
            if timing.get("name") == "tWR":
                raw_value &= 0xF
            return apply_formula(raw_value, timing.get("Formula"))

        if timing.get("address") is not None and "parameters" in timing:
            raw_value = read_timing(
                address=timing["address"],
                bit_start=timing["parameters"]["bit_start"],
                bit_length=timing["parameters"]["bit_length"],
                read_type="standard",
            )
            return apply_formula(raw_value, timing.get("Formula"))

        if timing.get("value") is not None:
            return resolve_display_value(timing["value"])
        if "default_value" in timing:
            return str(timing["default_value"])
        return "N/A"

    # Name, the two channels, and the spacer _align_dual_columns adds.
    ROW_FILL_SPAN = 4

    def _row_fill(self, body, row, bg, columns):
        """Carry a row's tint past the last value, to the end of the column.

        The labels only cover the first two columns, so a tint on them alone
        stops where the value stops. A CTkFrame spanning the row does not
        work: its canvas keeps the width it was created with rather than the
        cell's. An empty label in the trailing column does, and it is what the
        Timings tab already uses for the same job.
        """
        if bg == "transparent":
            return
        filler = ctk.CTkLabel(
            body, text="", height=self.ROW_HEIGHT, fg_color=bg,
            corner_radius=0,
        )
        # Spans the row rather than just the trailing column: the label and
        # value cells leave a one-pixel seam between them, which showed the
        # background through as a line down the column on shaded rows. Lowered
        # so the text labels keep drawing over it.
        #
        # Past the caller's own columns as well, because _align_dual_columns
        # adds a spacer column beyond them to hold the slack. Covering only
        # the value columns left the band stopping partway across the half --
        # 161px of a 378px column -- with bare background either side of it.
        # Tk clamps a span to the columns that exist, so a frame without the
        # spacer is unaffected.
        filler.grid(row=row, column=0, columnspan=max(columns, self.ROW_FILL_SPAN),
                    sticky="nsew")
        # Not lowered: a CTkFrame paints its own background on a child canvas,
        # so dropping to the bottom of the stacking order puts the stripe
        # behind that canvas and it disappears. Drawn first is enough -- the
        # row's labels are created after it and stack above.

    def _stripe_start(self, parent, show_header):
        """Where a section's zebra striping picks up from.

        A section with no header reads as a continuation of the list above it
        rather than a new one, so its shading has to continue the alternation.
        The middle Summary column is two sections -- the tertiary list and the
        tPHY block -- and restarting at zero there put the tPHY bands out of
        step with the columns either side.
        """
        if show_header:
            self._stripe_rows[parent] = 0
            return 0
        return self._stripe_rows.get(parent, 0)

    def _stripe_gap(self, show_header):
        """The gap under a section: none if the next one continues its list."""
        return (0, self.SECTION_GAP) if show_header else (0, 0)

    def _summary_paired_value(self, entry):
        """The slashed reading for one paired Summary row.

        Two names pair two separate rows, which is how the RTL entries read
        their R0 and R1. One name pairs that row's own channels instead: a
        DFE row already holds A and B, so pointing it at a second row would
        have nothing to point at.
        """
        def row_named(name):
            return next(
                (item for item in TIMINGS if item.get("name") == name), None
            )

        if len(entry) == 2:
            timing = row_named(entry[1])
            if timing is None:
                return "N/A"
            return "%s/%s" % (self._read_compact_side(timing, "a"),
                              self._read_compact_side(timing, "b"))

        first, second = row_named(entry[1]), row_named(entry[2])
        return "%s/%s" % (
            self._read_compact_value(first) if first else "N/A",
            self._read_compact_value(second) if second else "N/A",
        )

    def _compact_section(self, parent, title, categories, row, timing_names=None, label_overrides=None, show_header=True):
        """Create a dense timing list with minimal padding and no dead space."""
        label_overrides = label_overrides or {}
        if timing_names is not None:
            # Keep the exact requested order instead of relying on source category order.
            matching = []
            for requested_name in timing_names:
                if is_summary_pair(requested_name):
                    if any(
                        any(item.get("name") == wanted for item in TIMINGS)
                        for wanted in requested_name[1:]
                    ):
                        matching.append(requested_name)
                    continue
                timing = next(
                    (item for item in TIMINGS if item.get("name") == requested_name),
                    None,
                )
                if timing is not None:
                    matching.append(timing)
        else:
            matching = [
                timing for timing in TIMINGS
                if timing.get("Category") in categories
            ]
        if not matching:
            return row

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        # Full-width panel inside equal Summary columns. Name gutter is fixed;
        # values sit on the right edge so numbers line up cleanly (ATC style).
        section.grid(
            row=row, column=0, sticky="new", pady=self._stripe_gap(show_header)
        )
        section.grid_columnconfigure(0, weight=1)
        body_row = 1 if show_header else 0
        section.grid_rowconfigure(body_row, weight=0)

        if show_header:
            header = ctk.CTkLabel(
                section,
                text=title.upper(),
                font=self.HEADER_FONT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=2,
                text_color=self.SUBTITLE_COLOR,
                fg_color="transparent",
            )
            header.grid(row=0, column=0, sticky="ew")

        body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        body.grid(row=body_row, column=0, sticky="ew")
        body.grid_columnconfigure(0, weight=0, minsize=self.NAME_MINSIZE)
        body.grid_columnconfigure(1, weight=0, minsize=self.VALUE_MINSIZE)
        body.grid_columnconfigure(2, weight=1)

        index = 0
        data_row = self._stripe_start(parent, show_header)
        for timing in matching:
            body.grid_rowconfigure(index, weight=0)
            # Zebra striping: in a column this dense the eye loses which value
            # belongs to which label across the gap between them.
            bg = self.HIGHLIGHT_COLOR if data_row % 2 else "transparent"
            data_row += 1
            self._row_fill(body, index, bg, columns=3)

            if is_summary_pair(timing):
                label = timing[0]
                paired_value = self._summary_paired_value(timing)

                name = ctk.CTkLabel(
                    body, text=label,
                    font=self.COMPACT_FONT,
                    height=self.ROW_HEIGHT,
                    anchor="w", padx=self.ROW_PADX, pady=self.ROW_PADY,
                    text_color=self.TEXT_COLOR, fg_color=bg, bg_color=bg,
                )
                # Trailing pad, so the longest name in the column stops
                # short of its value instead of touching it.
                name.grid(row=index, column=0, sticky="nsew",
                          padx=(0, self.COLUMN_GAP))

                value = ctk.CTkLabel(
                    body, text=paired_value,
                    font=self.COMPACT_FONT,
                    height=self.ROW_HEIGHT,
                    anchor="w", justify="left", padx=self.VALUE_PADX, pady=self.ROW_PADY,
                    text_color=self.VALUE_COLOR, fg_color=bg, bg_color=bg,
                )
                value.grid(row=index, column=1, sticky="nsew")
                index += 1
                continue

            name = ctk.CTkLabel(
                body, text=label_overrides.get(timing.get("name"), timing.get("name", "")),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w", padx=self.ROW_PADX, pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR, fg_color=bg, bg_color=bg,
            )
            name.grid(row=index, column=0, sticky="nsew",
                      padx=(0, self.COLUMN_GAP))

            value = ctk.CTkLabel(
                body, text=self._read_compact_value(timing),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w", justify="left", padx=self.VALUE_PADX, pady=self.ROW_PADY,
                text_color=self._value_color(timing), fg_color=bg, bg_color=bg,
            )
            value.grid(row=index, column=1, sticky="nsew")
            self._register_live_value(timing, value)
            index += 1

        self._stripe_rows[parent] = data_row
        self._stripe_tail[parent] = (body, index)
        return row + 1

    def _compact_two_pair_section(self, parent, title, categories, row, timing_names=None, label_overrides=None, show_header=True):
        """Create a dense two-pair-per-row section to balance Summary column heights."""
        label_overrides = label_overrides or {}
        if timing_names is not None:
            matching = []
            for requested_name in timing_names:
                timing = next(
                    (item for item in TIMINGS if item.get("name") == requested_name),
                    None,
                )
                if timing is not None:
                    matching.append(timing)
        else:
            matching = [
                timing for timing in TIMINGS
                if timing.get("Category") in categories
            ]
        if not matching:
            return row

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section.grid(row=row, column=0, sticky="new", pady=(0, self.SECTION_GAP))
        section.grid_columnconfigure(0, weight=1)
        body_row = 1 if show_header else 0
        section.grid_rowconfigure(body_row, weight=0)

        if show_header:
            header = ctk.CTkLabel(
                section,
                text=title.upper(),
                font=self.HEADER_FONT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=2,
                text_color=self.SUBTITLE_COLOR,
                fg_color="transparent",
            )
            header.grid(row=0, column=0, sticky="ew")

        body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        body.grid(row=body_row, column=0, sticky="nsew")
        body.grid_columnconfigure(0, weight=3, uniform="vref_name")
        body.grid_columnconfigure(1, weight=1, uniform="vref_value")
        body.grid_columnconfigure(2, weight=3, uniform="vref_name")
        body.grid_columnconfigure(3, weight=1, uniform="vref_value")

        for index, timing in enumerate(matching):
            row_index = index // 2
            pair_index = index % 2
            name_column = pair_index * 2
            value_column = name_column + 1
            bg = "transparent"
            body.grid_rowconfigure(row_index, weight=0)

            name = ctk.CTkLabel(
                body,
                text=label_overrides.get(timing.get("name"), timing.get("name", "")),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR,
                fg_color=bg,
            )
            name.grid(row=row_index, column=name_column, sticky="nsew")

            value = ctk.CTkLabel(
                body,
                text=self._read_compact_value(timing),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="e",
                justify="right",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.VALUE_COLOR,
                fg_color=bg,
            )
            value.grid(row=row_index, column=value_column, sticky="nsew")

        return row + 1

    def _channel_headers(self, timing, default_a="ChA", default_b="ChB"):
        """Return the two channel column headings for a dual row.

        The slot name wins when one is known, so a column reads A2 rather than
        ChA; otherwise the row's own labels stand, which keeps a platform that
        reports no usable DeviceLocator on ChA/ChB.
        """
        labels = getattr(self, "channel_labels", {}) or {}
        a_text = timing.get("name_a", default_a) if timing else default_a
        b_text = timing.get("name_b", default_b) if timing else default_b
        return labels.get("a", a_text), labels.get("b", b_text)

    def _compact_dual_section(self, parent, title, categories, row, timing_names=None, label_overrides=None, show_header=True):
        """Create a dense Summary section with separate A/B values like the Skew tab."""
        label_overrides = label_overrides or {}
        if timing_names is not None:
            matching = []
            for requested_name in timing_names:
                timing = next(
                    (item for item in TIMINGS if item.get("name") == requested_name),
                    None,
                )
                if timing is not None:
                    matching.append(timing)
        else:
            matching = [
                timing for timing in TIMINGS
                if timing.get("Category") in categories
            ]
        if not matching:
            return row

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section.grid(
            row=row, column=0, sticky="new", pady=self._stripe_gap(show_header)
        )
        section.grid_columnconfigure(0, weight=1)
        body_row = 1 if show_header else 0
        section.grid_rowconfigure(body_row, weight=0)

        if show_header:
            header = ctk.CTkLabel(
                section,
                text=title.upper(),
                font=self.HEADER_FONT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=2,
                text_color=self.SUBTITLE_COLOR,
                fg_color="transparent",
            )
            header.grid(row=0, column=0, sticky="ew")

        body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        body.grid(row=body_row, column=0, sticky="ew")
        body.grid_columnconfigure(0, weight=0, minsize=self.NAME_MINSIZE)
        body.grid_columnconfigure(1, weight=0, minsize=self.VALUE_MINSIZE)
        body.grid_columnconfigure(2, weight=1)

        # No A2/B2 heading here: the values are slash pairs, which says the
        # same thing without spending a row on it, and a heading mid-column
        # broke the shading that runs across the tab.
        stripe = self._stripe_start(parent, show_header)
        for index, timing in enumerate(matching):
            body.grid_rowconfigure(index, weight=0)
            bg = self.HIGHLIGHT_COLOR if (stripe + index) % 2 else "transparent"
            self._row_fill(body, index, bg, columns=3)
            name = ctk.CTkLabel(
                body,
                text=label_overrides.get(timing.get("name"), timing.get("name", "")),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR,
                fg_color=bg, bg_color=bg,
            )
            name.grid(row=index, column=0, sticky="nw",
                      padx=(0, self.COLUMN_GAP))

            if is_dual_timing(timing):
                value_text = summary_slash_pair(
                    self._read_compact_side(timing, "a"),
                    self._read_compact_side(timing, "b"),
                )
            else:
                value_text = summary_compact_ohm(self._read_compact_value(timing))

            value = ctk.CTkLabel(
                body,
                text=value_text,
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                justify="left",
                padx=self.VALUE_PADX,
                pady=self.ROW_PADY,
                text_color=self.VALUE_COLOR,
                fg_color=bg, bg_color=bg,
            )
            value.grid(row=index, column=1, sticky="nw")

        self._stripe_rows[parent] = stripe + len(matching)
        self._stripe_tail[parent] = (body, len(matching))
        return row + 1

    def _summary_signal_section(self, parent, row, vref_names, vref_labels):
        """Combine RTT/ODT/RON/Drive Strength and VREF into one Summary panel."""
        rtt_timings = summary_signal_timings(TIMINGS)
        timing_by_name = {timing.get("name"): timing for timing in TIMINGS}

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section.grid(row=row, column=0, sticky="new", pady=(0, self.SECTION_GAP))
        section.grid_columnconfigure(0, weight=1)
        section.grid_rowconfigure(0, weight=0)
        section.grid_rowconfigure(1, weight=0)

        rtt_body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        rtt_body.grid(row=0, column=0, sticky="ew")
        rtt_body.grid_columnconfigure(0, weight=0, minsize=self.NAME_MINSIZE)
        rtt_body.grid_columnconfigure(1, weight=0, minsize=self.VALUE_MINSIZE)
        rtt_body.grid_columnconfigure(2, weight=1)

        # The channel heading is gone: every value here is already a slash
        # pair, and the row it used pushed this column out of step with the
        # shading in the other two.
        index = -1
        for timing in rtt_timings:
            index += 1
            rtt_body.grid_rowconfigure(index, weight=0)
            bg = self.HIGHLIGHT_COLOR if index % 2 else "transparent"
            self._row_fill(rtt_body, index, bg, columns=3)
            name_label = ctk.CTkLabel(
                rtt_body,
                text=timing.get("name", ""),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR,
                fg_color=bg, bg_color=bg,
            )
            name_label.grid(row=index, column=0, sticky="nsew",
                            padx=(0, self.COLUMN_GAP))

            # Summary-only shortening for every signal row (RTT, ODT, RON,
            # Drive Strength); the Timings tab still uses the full RZQ/… strings.
            # The category comes along because a driver that is off reads as
            # Hi-Z, while a termination that is off reads as zero.
            category = timing.get("Category")

            def _signal_text(raw, category=category):
                return summary_rtt_display(raw, category)

            if is_dual_timing(timing):
                value_text = summary_slash_pair(
                    _signal_text(self._read_compact_side(timing, "a")),
                    _signal_text(self._read_compact_side(timing, "b")),
                )
            else:
                value_text = summary_compact_ohm(
                    _signal_text(self._read_compact_value(timing))
                )

            value_label = ctk.CTkLabel(
                rtt_body,
                text=value_text,
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                justify="left",
                padx=self.VALUE_PADX,
                pady=self.ROW_PADY,
                text_color=self.VALUE_COLOR,
                fg_color=bg, bg_color=bg,
            )
            value_label.grid(row=index, column=1, sticky="nsew")

        # VREF continues in the grid the rows above use rather than a frame of
        # its own. Sized separately it could only line up by coincidence: its
        # longest name is shorter than "DQ/DQS ODT PARK", so its value column
        # started further left, and being proportional rather than fixed it
        # then pushed the numbers out to the right edge. Sharing the grid makes
        # the two blocks one column layout.
        #
        # One level per row, matching the Skew tab. The two-across pairing this
        # used to draw is gone: it read UP and DN side by side under relabelled
        # headings, which no other tab did.
        vref_timings = [timing_by_name[name] for name in vref_names if name in timing_by_name]
        for offset, timing in enumerate(vref_timings):
            index = len(rtt_timings) + offset
            rtt_body.grid_rowconfigure(index, weight=0)
            # Continues the striping of the block above rather than restarting.
            bg = self.HIGHLIGHT_COLOR if index % 2 else "transparent"
            self._row_fill(rtt_body, index, bg, columns=3)
            name_label = ctk.CTkLabel(
                rtt_body,
                text=vref_labels.get(timing.get("name"), timing.get("name", "")),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR,
                fg_color=bg, bg_color=bg,
            )
            name_label.grid(row=index, column=0, sticky="nsew",
                            padx=(0, self.COLUMN_GAP))
            value_label = ctk.CTkLabel(
                rtt_body,
                text=self._read_compact_value(timing),
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                justify="left",
                padx=self.VALUE_PADX,
                pady=self.ROW_PADY,
                text_color=self.VALUE_COLOR,
                fg_color=bg, bg_color=bg,
            )
            value_label.grid(row=index, column=1, sticky="nsew")

        drawn = len(rtt_timings) + len(vref_timings)
        self._stripe_rows[parent] = drawn
        self._stripe_tail[parent] = (rtt_body, drawn)
        return row + 1

    def _summary_system_memory_section(self, parent, timing_names, label_overrides=None, show_header=True):
        """Create a compact full-width About panel for the Summary tab."""
        label_overrides = label_overrides or {}
        timing_by_name = {
            timing.get("name"): timing for timing in TIMINGS
            if timing.get("name") in timing_names
        }

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section.grid(row=0, column=0, sticky="ew")
        section.grid_columnconfigure(0, weight=1)
        body_row = 1 if show_header else 0

        if show_header:
            header = ctk.CTkLabel(
                section,
                text="ABOUT",
                font=self.HEADER_FONT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=2,
                text_color=self.SUBTITLE_COLOR,
                fg_color="transparent",
            )
            header.grid(row=0, column=0, sticky="ew")

        row_layout = summary_system_memory_blocks(timing_by_name)

        # Rows are drawn in blocks of equally shaped rows, each with its own
        # grid, rather than one grid for the whole panel.
        #
        # A single grid gives every row the same column positions, so the widest
        # entry anywhere in a column sets where that column starts for all of
        # them. The motherboard string is far longer than any other value, and
        # it was pushing the second column right for the rows below it, taking
        # BCLK and Memory Capacity along with the BIOS beside it. Blocking
        # means a row only ever lines up with rows shaped like it.
        column_count = getattr(self, "summary_columns", SUMMARY_BASE_COLUMNS)

        # A rule where the panel changes subject: identity above, what the
        # memory is doing below, and one under the whole block before the
        # timing columns start.
        grid_row = body_row
        drawn_aligned = False
        # Every aligned row goes into one grid rather than one grid each.
        # They used to be a frame apiece, aligned afterwards by giving each
        # the same column minsize -- but a minsize is a floor, and the widest
        # rows pushed past it, so AGESA and Refresh Mode sat three pixels
        # right of BCLK and Memory Capacity and none of them met the timing
        # columns below. One grid makes it Tk's problem instead of a
        # measurement's.
        aligned_run = []
        for row_names, aligned in row_layout:
            is_aligned = aligned and 1 < len(row_names) <= column_count
            if is_aligned and not drawn_aligned:
                self._summary_rule(section, grid_row)
                grid_row += 1
                drawn_aligned = True

            # An aligned row is laid out on the Summary columns, so its first
            # entry starts where tCL does, its second where tREFI does, and
            # its third where RTT WR does. Every other row packs tight, each
            # entry beginning where the previous value ended.
            if is_aligned:
                aligned_run.append(row_names)
                continue
            body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
            body.grid(row=grid_row, column=0, sticky="ew")
            grid_row += 1
            self._summary_about_tight_row(
                body, row_names, timing_by_name, label_overrides,
            )
        if aligned_run:
            grid_row = self._summary_about_block(
                section, grid_row, aligned_run, timing_by_name,
                label_overrides, column_count,
            )
        self._summary_rule(section, grid_row, pady=(4, 2))

    def _summary_about_block(self, section, grid_row, rows, timing_by_name,
                             label_overrides, column_count):
        """Draw every aligned row into a single grid, and return the next row.

        The identity rows above keep their own frames: the board name is far
        longer than any other value, and sharing a grid with it would push the
        second column right for everything under it. Only rows shaped alike
        share.
        """
        body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        body.grid(row=grid_row, column=0, sticky="ew")
        for column in range(column_count):
            body.grid_columnconfigure(
                column, weight=1 if column == column_count - 1 else 0
            )
        # Widened to meet the timing columns once the tab is laid out; see
        # _align_summary_about. Guessing equal thirds here would only line up
        # by luck now that each column takes the width its own rows need.
        self._summary_about_rows.append(body)
        for offset, row_names in enumerate(rows):
            self._summary_about_aligned_row(
                body, row_names, timing_by_name, label_overrides,
                column_count, row=offset,
            )
        return grid_row + 1

    def _summary_rule(self, parent, row, pady=(3, 3)):
        """Draw a hairline across the panel, the way ZenTimings separates its
        header blocks from the grid below them."""
        # Two pixels, not one: a CTkFrame one pixel tall draws nothing at
        # all, which is how this first went in invisible. A CTkLabel renders
        # but reserves a whole text line, which is a bar rather than a rule.
        rule = ctk.CTkFrame(
            parent, height=2, corner_radius=0, fg_color=self.RULE_COLOR
        )
        rule.grid(row=row, column=0, sticky="ew", padx=self.ROW_PADX, pady=pady)
        return rule

    def _summary_about_pair(self, parent, timing, label_overrides, column):
        """Draw one label/value pair, returning the next free column."""
        label_text = label_overrides.get(timing.get("name"), timing.get("name", ""))
        if label_text:
            name_label = ctk.CTkLabel(
                parent,
                text=label_text,
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w", padx=self.ROW_PADX, pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR, fg_color="transparent",
            )
            name_label.grid(row=0, column=column, sticky="w")
            column += 1
        value_label = ctk.CTkLabel(
            parent,
            text=self._read_compact_value(timing),
            font=self.COMPACT_FONT,
            height=self.ROW_HEIGHT,
            anchor="w", justify="left",
            padx=self.ROW_PADX, pady=self.ROW_PADY,
            text_color=self.VALUE_COLOR, fg_color="transparent",
        )
        value_label.grid(row=0, column=column, sticky="w")
        # Drawn once and never looked at again, which is why a lost first read
        # stuck here: this is the strip UCLK:MCLK sat blank on.
        self._register_live_value(timing, value_label)
        return column + 1

    def _summary_about_aligned_row(self, body, row_names, timing_by_name,
                                   label_overrides, column_count, row=0):
        """Put one entry per Summary column, on those columns.

        ``body`` is shared with the other aligned rows and ``row`` says which
        line of it this is, so the columns are the same columns rather than
        three grids measured into agreeing. See _summary_about_block.
        """
        for column, timing_name in enumerate(row_names):
            # A hole keeps the columns after it in place; the cell stays empty.
            timing = timing_by_name.get(timing_name) if timing_name else None
            if timing is None:
                continue
            cell = ctk.CTkFrame(body, corner_radius=0, fg_color="transparent")
            cell.grid(
                row=row,
                column=column,
                sticky="ew",
                # The whole gap goes on the right. Splitting it left and
                # right inset every cell by four pixels, so the strip started
                # four pixels inside the column it is meant to start on --
                # and no amount of widening the column could close that,
                # because the offset was inside the cell.
                padx=(0, 0 if column == column_count - 1 else 8),
            )
            self._summary_about_pair(cell, timing, label_overrides, 0)

    def _summary_about_tight_row(self, body, row_names, timing_by_name,
                                 label_overrides):
        """Pack the entries left to right, each starting where the last ended."""
        column = 0
        for timing_name in row_names:
            timing = timing_by_name.get(timing_name)
            if timing is None:
                continue
            column = self._summary_about_pair(
                body, timing, label_overrides, column
            )
        # Everything left over goes to a trailing filler so the pairs stay
        # together at the left rather than spreading across the strip.
        body.grid_columnconfigure(column, weight=1)

    def _summary_rtl_section(self, parent, row):
        """Show Summary RTL values in the four paired rows used by ASRock Timing Configurator."""
        timing_by_name = {timing.get("name"): timing for timing in TIMINGS}
        rtl_rows = list(SUMMARY_RTL_ROWS)

        section = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section.grid(row=row, column=0, sticky="ew", pady=(0, 7))
        section.grid_columnconfigure(0, weight=1)

        header = ctk.CTkLabel(
            section,
            text="RTL",
            font=self.HEADER_FONT,
            anchor="w",
            padx=self.ROW_PADX,
            pady=2,
            text_color=self.SUBTITLE_COLOR,
            fg_color="transparent",
        )
        header.grid(row=0, column=0, sticky="ew")

        body = ctk.CTkFrame(section, corner_radius=0, fg_color="transparent")
        body.grid(row=1, column=0, sticky="ew")
        body.grid_columnconfigure(0, weight=2)
        body.grid_columnconfigure(1, weight=1)

        # Rows the platform does not carry are skipped rather than shown as
        # N/A: Arrow Lake has no DFE block at all, and four dead rows there
        # would say less than none.
        entries = [(entry[0], self._summary_paired_value(entry))
                   for entry in rtl_rows + list(SUMMARY_DFE_BIAS_ROWS)
                   if any(name in timing_by_name for name in entry[1:])]

        for index, (label, paired_value) in enumerate(entries):
            bg = "transparent"

            name_label = ctk.CTkLabel(
                body,
                text=label,
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR,
                fg_color=bg,
            )
            name_label.grid(row=index, column=0, sticky="ew")

            value_label = ctk.CTkLabel(
                body,
                text=paired_value,
                font=self.COMPACT_FONT,
                height=self.ROW_HEIGHT,
                anchor="e",
                justify="right",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.VALUE_COLOR,
                fg_color=bg,
            )
            value_label.grid(row=index, column=1, sticky="ew")

        return row + 1

    def build_summary_tab(self):
        """Build the full-width system strip and dense three-column Summary view."""
        summary_frames = self.grid_frames["Summary"]
        columns = summary_frames["Columns"]

        system_memory_names = summary_system_memory_names()
        self._summary_system_memory_section(
            summary_frames["FullWidth"],
            system_memory_names,
            # CPU and Model carry their own names in the value, so a
            # label beside them repeats it. Blanked rather than
            # dropped: the pair keeps its place in the row, and
            # _summary_about_pair already skips an empty label.
            label_overrides={"Uncore": "Ring", "CPU": "", "Model": ""},
            show_header=False,
        )

        available_names = {timing.get("name") for timing in TIMINGS}
        if "Gear Down Mode" in available_names:
            primary_secondary_names, tertiary_names = am5_summary_timing_columns(TIMINGS)
        else:
            primary_secondary_names, tertiary_names = (
                intel_summary_timing_columns(TIMINGS)
            )

        # The command rate reads as a timing, so it belongs under tRC rather
        # than in the system strip. Named both ways because the platforms
        # spell the row differently; the one that does not exist here is
        # skipped when the section looks its row up.
        primary_secondary_names = insert_summary_rows_after(
            primary_secondary_names, "tRC", ("CR",)
        )

        # VREF is listed the way the Skew tab lists it: one level per row, in
        # the table's own order, taken from the same category so the two
        # displays cannot drift apart. Summary previously kept its own name
        # order and relabelled every row, which is why the two tabs disagreed
        # about what these are called.
        summary_vref_names = insert_summary_rows_after(
            summary_vref_row_names(TIMINGS),
            SUMMARY_SIGNAL_TAIL_ANCHOR,
            SUMMARY_SIGNAL_TAIL_ROWS,
        )

        # The Skew tab's names, unaltered.
        summary_vref_labels = {}

        signal_row_count = len(summary_signal_timings(TIMINGS)) + len(summary_vref_names)
        if signal_row_count:
            third_column_sections = [{
                "combined_signal": True,
                "stretch_weight": max(1, signal_row_count + 1),
            }]
        else:
            third_column_sections = [{
                "title": "AM5 Control",
                "categories": ("AM5 Control",),
                "show_header": False,
            }]

        middle_sections = [{
            "title": "Tertiary",
            "categories": ("Tertiary", "Refresh timings"),
            "timing_names": tertiary_names,
            "show_header": False,
        }]
        # Always pin the precharge/mode/power-down group at the end of
        # column 0 so those rows cannot disappear if category filters drift.
        #
        # The order comes from AM5_SUMMARY_TIMING_PRIORITY rather than being
        # written again here. Held separately, the two disagreed about where
        # tCKE and tXP go, and this list is applied second, so it silently won.
        timing_tail = [
            name for name in AM5_SUMMARY_TIMING_PRIORITY
            if name in SUMMARY_COLUMN_TAIL and name in available_names
        ]
        if "Gear Down Mode" in available_names and timing_tail:
            primary_secondary_names = [
                name for name in primary_secondary_names if name not in set(timing_tail)
            ] + timing_tail

        phy_names = [name for name in AM5_SUMMARY_PHY_NAMES if name in available_names]
        if phy_names and "Gear Down Mode" in available_names:
            # Dual ChA/ChB PHY block under tertiary (all three tPHY* rows).
            middle_sections.append({
                "title": "PHY",
                "categories": ("Tertiary",),
                "timing_names": phy_names,
                "dual_values": True,
                "show_header": False,
            })

        column_sections = [
            [{
                "title": "Primary / Secondary",
                "categories": ("Primary", "Secondary"),
                "timing_names": primary_secondary_names,
                "show_header": False,
            }],
            middle_sections,
            third_column_sections,
        ]

        # The rails used to have a column here. They are readings rather than
        # settings, so they moved to the Sensor Telemetry window where each one
        # carries what it has done over time.
        for column, sections in zip(columns, column_sections):
            row = 0
            for section in sections:
                # Keep natural row height; leftover vertical space stays empty
                # under top-aligned columns instead of inflating every timing row.
                column.grid_rowconfigure(row, weight=0)
                if section.get("summary_rtl"):
                    row = self._summary_rtl_section(column, row)
                    continue
                if section.get("combined_signal"):
                    row = self._summary_signal_section(
                        column,
                        row,
                        summary_vref_names,
                        summary_vref_labels,
                    )
                    continue
                if section.get("dual_values"):
                    row = self._compact_dual_section(
                        column,
                        section["title"],
                        section["categories"],
                        row,
                        timing_names=section.get("timing_names"),
                        label_overrides=section.get("label_overrides"),
                        show_header=section.get("show_header", True),
                    )
                    continue
                if section.get("two_pair"):
                    row = self._compact_two_pair_section(
                        column,
                        section["title"],
                        section["categories"],
                        row,
                        timing_names=section.get("timing_names"),
                        label_overrides=section.get("label_overrides"),
                        show_header=section.get("show_header", True),
                    )
                    continue
                row = self._compact_section(
                    column,
                    section["title"],
                    section["categories"],
                    row,
                    timing_names=section.get("timing_names"),
                    label_overrides=section.get("label_overrides"),
                    show_header=section.get("show_header", True),
                )

        self._extend_summary_shading()

    def load_all_tabs_content(self):
        """Load content for all tabs dynamically based on TIMINGS."""
        self.build_summary_tab()
        for tab_name in self.tab_names:
            if tab_name == "Summary":
                continue
            # Diagnostic rows are summarised into one line on the tab, and
            # kept in full in TIMINGS, which is where anyone chasing a blank
            # reading will look.
            tab_timings = [
                t for t in TIMINGS
                if t["Tab"] == tab_name and not t.get("diagnostic")
            ]
            categories = []
            current_category = None
            for timing in tab_timings:
                category = timing["Category"]
                if current_category != category:
                    categories.append((category, []))
                    current_category = category
                categories[-1][1].append(timing["name"])
            left_column = []
            right_column = []
            # A tab whose rows all name one column is drawn as one column,
            # the channel-pinned latency blocks included -- otherwise Misc
            # asks for a single column and still draws Latency CHB beside it.
            single_column = not any(
                timing.get("Column") == "Right" for timing in tab_timings
            )
            for cat, names in categories:
                timing_entry = next((t for t in tab_timings if t["Category"] == cat), None)
                column = timing_entry.get("Column", "Left") if timing_entry else "Left"
                # The latency blocks are pinned by channel wherever they land.
                # They no longer have a tab to themselves, so keying this on
                # the tab name would have quietly stopped applying when they
                # moved in beside the Misc sections.
                if single_column:
                    column = "Left"
                elif cat == "Latency CHA":
                    column = "Left"
                elif cat == "Latency CHB":
                    column = "Right"
                if column == "Left":
                    left_column.append((cat, names))
                else:
                    right_column.append((cat, names))
            if tab_name == "Timings":
                left_column = ordered_sections(left_column, TIMINGS_SECTION_ORDER)
                right_column = ordered_sections(right_column, TIMINGS_SECTION_ORDER)
            # Stack sections naturally from top to bottom. The older layout tried
            # to equalize both column heights by adding large amounts of padding
            # to the shorter column, which created visible dead space on wide or
            # maximized windows.
            section_external_padding = 4
            # A continuous tab is one table per column: the channel header sits
            # once at the top, the section names are rows, and nothing pads
            # between sections. Every row is then the same pitch from the same
            # origin, so the two columns line up without being forced to.
            continuous = tab_name in CONTINUOUS_SECTION_TABS
            if continuous:
                section_external_padding = 0

            def draw_column(parent, sections):
                """Stack one column's sections, banding them as one table."""
                if not continuous:
                    # The band runs down the whole column, not down each
                    # section. Restarting it per section put two rows of the
                    # same shade against each other at every seam that
                    # followed an odd-length section, which reads as no
                    # shading at all rather than as a new block.
                    band = 0
                    for index, (section_name, timing_names) in enumerate(sections):
                        self.create_section(
                            parent, section_name, timing_names,
                            column=0, row=index, extra_pady=0,
                            return_frame=True, tab_name=tab_name,
                            pady=(section_external_padding,
                                  section_external_padding),
                            band_offset=band,
                        )
                        band += len(
                            self._section_rows(section_name, timing_names)
                        )
                    return

                # The first section heading is row 0 of the column in
                # both halves; the channel names ride on the headings
                # themselves rather than on a row of their own above
                # them, so there is nothing before them to count.
                grid_row = 0
                band = 0
                for section_name, timing_names in sections:
                    self.create_section(
                        parent, section_name, timing_names,
                        column=0, row=grid_row, extra_pady=0,
                        return_frame=True, tab_name=tab_name,
                        pady=(0, 0),
                        show_channel_header=False,
                        uniform_header=True,
                        band_offset=band,
                    )
                    grid_row += 1
                    band += 1 + len(
                        self._section_rows(section_name, timing_names)
                    )

            draw_column(self.grid_frames[tab_name]["Left"], left_column)

            draw_column(self.grid_frames[tab_name]["Right"], right_column)

        self._align_dual_columns()
        for shaded_tab in PAIRED_SECTION_TABS:
            self._pair_section_rows(shaded_tab)
        for continuous_tab in CONTINUOUS_SECTION_TABS:
            self._match_heading_pitch(continuous_tab)
        for shaded_tab in SHADED_TABS:
            self._extend_column_shading(shaded_tab)
        self._align_summary_value_columns()
        self._align_summary_about()

    def _section_rows(self, section_name, timing_names):
        """The timings a section will actually draw, in order.

        A name with no matching row is skipped when the section is built, so
        the caller counting band positions has to skip it too or the shading
        walks out of step with the rows.
        """
        rows = []
        for timing_name in timing_names:
            timing = next(
                (
                    t for t in TIMINGS
                    if t["name"].lower() == timing_name.lower()
                    and t["Category"] == section_name
                ),
                None,
            )
            if timing is not None:
                rows.append(timing)
        return rows

    def _match_heading_pitch(self, tab_name):
        """Give every row on a continuous tab the same height.

        The two columns line up only while every row is the same pitch, and
        three things break that on their own:

        A heading built to ROW_HEIGHT is shorter than a data row, because a
        data row is as tall as its value labels and those carry their own
        padding. One heading's worth is invisible; the Timings columns hold
        four headings against two, so the foot of the tab sat four pixels out.

        A dual-channel row and a single-value row are built by different
        branches with different padding, so they come out different heights.
        On Skew the left column is dual at 22 pixels and the right single at
        20, and by the foot of the tab that is a row and a half of drift.

        So the tallest row on the tab is measured and everything -- headings
        and rows, both columns, both branches -- is held to it. Measured for
        the same reason _pair_section_rows measures its spacers: the height
        depends on fonts and padding a constant here would only keep guessing.
        """
        headers = self._section_headers.get(tab_name) or []
        sections = self._section_bodies.get(tab_name) or []
        if not sections:
            return
        self.root.update_idletasks()

        pitch = 0
        for section in sections:
            for data_row in range(section["drawn"]):
                bbox = section["body"].grid_bbox(
                    0, section["first_row"] + data_row
                )
                if bbox and bbox[3]:
                    pitch = max(pitch, bbox[3])
        if not pitch:
            return
        for section in sections:
            body = section["body"]
            for data_row in range(section["drawn"]):
                body.grid_rowconfigure(
                    section["first_row"] + data_row, weight=0, minsize=pitch
                )
        for header in headers:
            header.configure(height=pitch)

    def _extend_summary_shading(self):
        """Carry the row bands down to the foot of the tallest column.

        The columns do not hold the same number of rows -- the middle one
        ends with three tPHY rows the others have nothing to put beside --
        so the last bands stopped where the shorter columns ran out and the
        shading covered only the left two thirds of the tab. Blank rows keep
        the alternation going to the bottom, so every band is as wide as the
        one above it.
        """
        columns = (self.grid_frames.get("Summary") or {}).get("Columns") or []
        tails = [self._stripe_tail.get(column) for column in columns]
        if not columns or any(tail is None for tail in tails):
            return
        depth = max(self._stripe_rows.get(column, 0) for column in columns)
        for column, (body, next_row) in zip(columns, tails):
            drawn = self._stripe_rows.get(column, 0)
            for step in range(depth - drawn):
                # Built here rather than through _row_fill, which skips the
                # unshaded rows: with no labels to hold them open those rows
                # would collapse to nothing and pull the shaded ones up out
                # of step with the columns beside them.
                bg = (self.HIGHLIGHT_COLOR if (drawn + step) % 2
                      else "transparent")
                body.grid_rowconfigure(next_row + step, weight=0)
                spacer = ctk.CTkLabel(
                    body, text="", height=self.ROW_HEIGHT, fg_color=bg,
                    corner_radius=0,
                )
                spacer.grid(
                    row=next_row + step, column=0, columnspan=3, sticky="nsew"
                )

    def _extend_column_shading(self, tab_name):
        """Carry the bands to the foot of the taller column.

        The two columns hold different numbers of rows -- Timings has six more
        on the right than the left, Skew three more on the left -- so below
        wherever the shorter one ends, the band covered half the tab and
        stopped. The rows there are still rows of the same table and take the
        same shade across it.

        Blank rows rather than a taller fill: a fill would have to know where
        the section below it starts, while a row keeps the alternation going
        by being one, and the pitch is measured from a real row for the same
        reason _match_heading_pitch measures its own.
        """
        sections = self._section_bodies.get(tab_name) or []
        if not sections:
            return
        self.root.update_idletasks()

        halves = {}
        for section in sections:
            halves.setdefault(section["body"].winfo_rootx(), []).append(section)
        if len(halves) != 2:
            return

        pitch = 0
        feet = {}
        for x, group in halves.items():
            for section in group:
                body = section["body"]
                for data_row in range(section["drawn"]):
                    bbox = body.grid_bbox(0, section["first_row"] + data_row)
                    if bbox and bbox[3]:
                        pitch = max(pitch, bbox[3])
                bottom = body.winfo_rooty() + body.winfo_height()
                if bottom > feet.get(x, (0, None))[0]:
                    feet[x] = (bottom, section)
        if not pitch or len(feet) != 2:
            return

        deepest = max(bottom for bottom, _section in feet.values())
        for x, (bottom, section) in feet.items():
            missing = int(round((deepest - bottom) / float(pitch)))
            if missing <= 0:
                continue
            body = section["body"]
            next_row = section["first_row"] + section["drawn"]
            # Continue this column's own alternation, read from the row
            # it ends on rather than computed. The band a row takes
            # comes from row_band, whose offset is not kept here, and a
            # grid index is not that number -- using one put every
            # added row on the wrong shade.
            shaded = any(
                "#" in str(child.cget("fg_color"))
                for child in body.grid_slaves(row=next_row - 1)
                if hasattr(child, "cget")
            )
            for step in range(missing):
                shaded = not shaded
                bg = self.HIGHLIGHT_COLOR if shaded else "transparent"
                body.grid_rowconfigure(next_row + step, weight=0, minsize=pitch)
                spacer = ctk.CTkLabel(
                    body, text="", height=pitch, fg_color=bg, corner_radius=0,
                )
                spacer.grid(row=next_row + step, column=0,
                            columnspan=self.ROW_FILL_SPAN, sticky="nsew")

    def _align_summary_value_columns(self):
        """Give every section in a Summary column one name-column width.

        Each section owns its grid and sized column 0 from its own longest
        name, so a section whose names are shorter put its values further
        left than the section above it. The PHY block was the visible case:
        tPHYWRD is narrower than tREFI, so its slash pairs sat 21px left of
        every other value in the middle column.

        Measured rather than fixed, for the same reason the About strip is:
        the width depends on the fonts and the padding the labels carry.
        """
        columns = (self.grid_frames.get("Summary") or {}).get("Columns") or []
        if not columns:
            return
        self.root.update_idletasks()

        for column in columns:
            bodies = []

            def collect(widget):
                for child in widget.winfo_children():
                    try:
                        slaves = child.grid_slaves()
                    except Exception:
                        slaves = []
                    used = {int((slave.grid_info() or {}).get("column", -1))
                            for slave in slaves}
                    # A row body is a grid holding a name and a value beside
                    # it. The section frames above them hold one child in
                    # column 0 and are skipped by the same test.
                    if {0, 1} <= used:
                        bodies.append(child)
                    collect(child)

            collect(column)
            widest = 0
            for body in bodies:
                for slave in body.grid_slaves():
                    info = slave.grid_info() or {}
                    if int(info.get("column", -1)) == 0:
                        widest = max(
                            widest,
                            slave.winfo_reqwidth() + _grid_padx(info),
                        )
            if not widest:
                continue
            for body in bodies:
                body.grid_columnconfigure(0, weight=0, minsize=widest)

    def _align_summary_about(self):
        """Size each Summary column to fit both the About row and the timings.

        The panel above the grid only reads as one layout if its cells start
        where the columns under them start. Aligning it to the timing columns
        alone is not enough: the About entries can be the wider of the two --
        the AGESA string is half again the width of the middle column's
        timings -- so each column takes whichever side needs more, and both
        are then given that width.
        """
        summary = self.grid_frames.get("Summary", {})
        columns = summary.get("Columns") or []
        if not columns or not self._summary_about_rows:
            return
        self.root.update_idletasks()

        widths = [column.winfo_reqwidth() for column in columns]
        for body in self._summary_about_rows:
            for cell in body.grid_slaves():
                info = cell.grid_info()
                index = int(info.get("column", 0))
                if index < len(widths):
                    # The gap around the cell counts. Measuring the cell alone
                    # set a minsize the column then grew past to fit the pads,
                    # which is why the strip sat seven pixels right of the
                    # timing columns it is supposed to start on.
                    widths[index] = max(
                        widths[index],
                        cell.winfo_reqwidth() + _grid_padx(info),
                    )

        last = len(widths) - 1
        parent = columns[0].master
        for index, width in enumerate(widths):
            width = summary_column_width(
                width, is_last=index == last, gap=self.COLUMN_GAP
            )
            parent.grid_columnconfigure(
                index, minsize=width, weight=1 if index == last else 0
            )
            for body in self._summary_about_rows:
                body.grid_columnconfigure(
                    index, minsize=width, weight=1 if index == last else 0
                )

    def _tab_has_dual_section(self, tab_name):
        """True when any row on the tab reads two channels.

        Decided per tab rather than per section: the columns only have to
        agree with each other, and a tab with no dual row anywhere keeps the
        tighter name-width layout it has always had.
        """
        return any(
            is_dual_timing(timing) for timing in TIMINGS
            if timing.get("Tab") == tab_name
        )

    # Breathing room after the longest entry in a column, before the
    # column beside it: the name column against its first value, and
    # the channel-A column against channel B. Shorter entries in the
    # same column get more, which is what a shared column means.
    COLUMN_GAP = 25

    def _align_dual_columns(self):
        """Line the ChA/ChB columns of every dual section on a tab up with each other.

        Each section owns its own grid, so the three columns were sized from
        that section's own longest name and value: a wide row such as
        "117 ns / 95 ns" under Refresh Timings or "RZQ/6 (40 Ω)" under RTT
        pushed its channel headers away from the sections above and below.
        Giving every section on the tab the widest column widths found on that
        tab makes the requested widths identical, so the leftover width is
        split the same way everywhere and the columns end up at the same x.
        """
        self.root.update_idletasks()
        for frames in self._dual_content_frames.values():
            widths = [0, 0, 0]
            for frame in frames:
                for child in frame.grid_slaves():
                    column = int(child.grid_info().get("column", 0))
                    if column < len(widths):
                        widths[column] = max(widths[column], child.winfo_reqwidth())
            # Each column is sized to its own longest entry, which left
            # that one row hard against the column beside it with
            # nothing between them. The gap goes on the column rather
            # than the label, so every value in the half still starts
            # at one x.
            #
            # Only columns that have something after them: a half whose
            # sections all read one value leaves the channel-B column
            # empty, and padding the column before it bought a gap from
            # nothing to nothing, which then stacked with the gap
            # between the halves and made Misc's 50px instead of 25.
            last_used = max(
                (index for index, width in enumerate(widths) if width),
                default=0,
            )
            for column in range(last_used):
                widths[column] += self.COLUMN_GAP
            for frame in frames:
                for column, width in enumerate(widths):
                    # No weight, so the measured width is the width. With the
                    # 4:1:1 the sections are built with, every pixel the
                    # window has over the content went to the name column and
                    # the value drifted away from the name it belongs to --
                    # the wider the window, the further.
                    frame.grid_columnconfigure(column, weight=0, minsize=width)
                # The slack goes here instead, past the last channel, so the
                # three columns stay packed together at the left.
                frame.grid_columnconfigure(len(widths), weight=1)

    @staticmethod
    def row_band(band_offset, uniform_header, data_row):
        """Which band position a section's Nth data row occupies.

        Both branches of create_section -- the dual-channel one and the
        single-value one -- have to answer this identically, or a column of
        single-value sections runs out of step with the dual column facing it
        and every row on the tab comes out one shade on the left and the other
        on the right. Written once here for that reason.

        On a continuous tab the section heading is itself a row of the table
        and holds a band position, which is what the uniform_header term is.
        """
        return band_offset + (1 if uniform_header else 0) + data_row

    def _shade_row(self, tab_name, data_row):
        """The background for one row of a generic section.

        Only the tabs in SHADED_TABS band their rows; everywhere else this
        returns the transparent background those sections have always used.
        """
        if tab_name in SHADED_TABS and data_row % 2:
            return self.HIGHLIGHT_COLOR
        return "transparent"

    def _register_section_body(self, tab_name, row, body, first_row, drawn):
        """Note where a section's rows live, for the pairing pass below."""
        self._section_bodies.setdefault(tab_name, []).append(
            {"row": row, "body": body, "first_row": first_row, "drawn": drawn}
        )

    def _pair_section_rows(self, tab_name):
        """Give the two halves of a banded tab the same rows at the same y.

        The sections facing each other hold different numbers of rows -- six
        primaries against five RTT values -- so the shorter one ran out and
        its neighbour's bands crossed only half the tab. Blank rows bring the
        shorter section up to the taller one, which lines up every row that
        follows as well: the next pair of headings then starts at the same y
        on both sides.

        The row height is measured rather than assumed. A row is a label of
        ROW_HEIGHT plus whatever padding the value labels carry, and a spacer
        built to the nominal height alone would come up short and walk the
        two halves out of step.
        """
        sections = self._section_bodies.get(tab_name) or []
        if not sections:
            return
        self.root.update_idletasks()

        pitch = 0
        for section in sections:
            if section["drawn"]:
                bbox = section["body"].grid_bbox(0, section["first_row"])
                if bbox and bbox[3]:
                    pitch = max(pitch, bbox[3])
        if not pitch:
            return

        by_row = {}
        for section in sections:
            by_row.setdefault(section["row"], []).append(section)
        for facing in by_row.values():
            target = max(section["drawn"] for section in facing)
            for section in facing:
                body = section["body"]
                for data_row in range(section["drawn"], target):
                    bg = self._shade_row(tab_name, data_row)
                    grid_row = section["first_row"] + data_row
                    body.grid_rowconfigure(grid_row, weight=0, minsize=pitch)
                    spacer = ctk.CTkLabel(
                        body, text="", height=pitch, fg_color=bg,
                        corner_radius=0,
                    )
                    spacer.grid(
                        row=grid_row, column=0, columnspan=3, sticky="nsew"
                    )

    def create_section(self, parent, section_name, timing_names, column=0, row=0, columnspan=1, extra_pady=0, return_frame=False, tab_name=None, pady=(2, 2), show_channel_header=True, uniform_header=False, band_offset=0):
        """Create a categorized section block with consistent layout for single or dual-channel timings.

        ``show_channel_header`` draws the A1/B1 pair above the values. A column
        of sections wants it once, at the top of the column, not repeated over
        every block.

        ``uniform_header`` makes the section name exactly one row tall and
        bands it like any other row. Together with ``band_offset``, which is
        how many rows the column has already drawn, that turns a stack of
        sections into one continuous table: every row is the same pitch from
        the same origin, so row N sits at the same y and takes the same shade
        in both columns, whatever each section happens to hold.
        """
        section_frame = ctk.CTkFrame(
            parent,
            corner_radius=0,
            border_width=0,
            fg_color="transparent",
        )
        section_frame.grid(
            row=row,
            column=column,
            columnspan=columnspan,
            # A banded tab has no side margin: the two halves meet in the
            # middle so a row's shading crosses the tab in one piece rather
            # than stopping either side of a gutter.
            padx=0 if tab_name in SHADED_TABS else 3,
            pady=pady,
            sticky="new"
        )
        section_frame.grid_columnconfigure(0, weight=1)
        section_frame.grid_rowconfigure(1, weight=0)
        # Whether this section reads two channels decides whether its
        # heading names them, so it is settled before the heading.
        heading_is_dual = any(
            is_dual_timing(timing)
            for timing in self._section_rows(section_name, timing_names)
        )
        # The heading is a row of the table, so it takes the band its position
        # calls for rather than a colour of its own.
        heading_background = (
            self._shade_row(tab_name, band_offset) if uniform_header
            else "transparent"
        )
        header_frame = ctk.CTkFrame(
            section_frame,
            corner_radius=0,
            fg_color=heading_background,
        )
        header_frame.grid(row=0, column=0, sticky="ew", padx=0, pady=0)
        header_kwargs = {"pady": 2}
        if uniform_header:
            # One row tall, so the heading counts as a row like any other and
            # the two columns stay in step. The band carries across the label
            # as well as the frame, or the text sits in a hole in the band.
            header_kwargs = {
                "pady": self.ROW_PADY,
                "height": self.ROW_HEIGHT,
                "fg_color": heading_background,
                "bg_color": heading_background,
            }
        header = ctk.CTkLabel(
            header_frame,
            text=section_name.upper(),
            font=self.HEADER_FONT,
            anchor="w",
            padx=self.ROW_PADX,
            # On a continuous tab the heading is a row among rows, so it takes
            # the value colour to stand out from the names beside it. The
            # muted subtitle colour reads as a dimmed row there rather than as
            # a heading; on a blocked tab it still separates the block title
            # from its contents, which is what it is for.
            text_color=(
                self.VALUE_COLOR if uniform_header else self.SUBTITLE_COLOR
            ),
            **header_kwargs,
        )
        if uniform_header and heading_is_dual:
            # The channel names belong to the section, not to the tab:
            # they sit on the heading, over the values they name, so a
            # section read on its own says which column is which.
            header_frame.grid_columnconfigure(0, weight=0)
            header.grid(row=0, column=0, sticky="ew")
            a_text, b_text = self._channel_headers(
                next(iter(self._section_rows(section_name, timing_names))),
                "A", "B",
            )
            for column, text in ((1, a_text), (2, b_text)):
                ctk.CTkLabel(
                    header_frame, text=text, font=self.HEADER_FONT,
                    anchor="w", padx=self.ROW_PADX,
                    text_color=self.SUBTITLE_COLOR,
                    **header_kwargs,
                ).grid(row=0, column=column, sticky="ew")
            # Same alignment group as the rows, so A1 sits over the
            # channel-A values rather than wherever its own text ends.
            self._dual_content_frames.setdefault(
                (tab_name, id(parent)), []).append(header_frame)
        else:
            header.pack(fill="x", expand=True)
        if uniform_header:
            # Height is corrected against a real row later; see
            # _match_heading_pitch. ROW_HEIGHT is only the starting guess.
            self._section_headers.setdefault(tab_name, []).append(header)
        content_frame = ctk.CTkFrame(
            section_frame,
            corner_radius=0,
            fg_color="transparent",
            border_width=0,
        )
        content_frame.grid(row=1, column=0, sticky="nsew", padx=0, pady=0)
        section_timings = [t for t in TIMINGS if t["Category"] == section_name and t["name"].lower() in [tn.lower() for tn in timing_names]]
        has_dual_addresses = any(
            is_dual_timing(timing) for timing in section_timings
        )
        if has_dual_addresses:
            content_frame.grid_columnconfigure(0, weight=4)
            content_frame.grid_columnconfigure(1, weight=1)
            content_frame.grid_columnconfigure(2, weight=1)
            # Collected so _align_dual_columns can give every dual section in
            # this half the same column widths. Keyed by the half rather than
            # by the tab: the two halves carry their own A1/B1 headers and do
            # not have to share a name width, and sharing one meant the
            # longest name anywhere on the tab set the gap for every row in
            # both columns -- tCL sat 169px from its value because
            # CounttREFIWhileRefEnOff is in the other column.
            self._dual_content_frames.setdefault(
                (tab_name, id(parent)), []).append(content_frame)
            first_dual_timing = next(
                (t for t in section_timings if is_dual_timing(t)),
                None
            )
            if first_dual_timing:
                parameter_header_text = first_dual_timing.get("parameter_name", "Channel")
                if str(parameter_header_text).strip().lower() == "name":
                    parameter_header_text = ""
            else:
                parameter_header_text = "Channel"
            a_header_text, b_header_text = self._channel_headers(
                first_dual_timing, "A", "B"
            )
            # Without the channel header the values start at the top of the
            # content grid instead of below it.
            first_data_row = 1 if show_channel_header else 0
            parameter_header = ctk.CTkLabel(
                content_frame,
                text=parameter_header_text,
                font=self.COMPACT_BOLD,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=self.ROW_PADX,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR
            )
            parameter_header.grid(row=0, column=0, sticky="ew")
            a_header = ctk.CTkLabel(
                content_frame,
                text=a_header_text,
                font=self.COMPACT_BOLD,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=4,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR
            )
            a_header.grid(row=0, column=1, sticky="ew")
            b_header = ctk.CTkLabel(
                content_frame,
                text=b_header_text,
                font=self.COMPACT_BOLD,
                height=self.ROW_HEIGHT,
                anchor="w",
                padx=4,
                pady=self.ROW_PADY,
                text_color=self.TEXT_COLOR
            )
            b_header.grid(row=0, column=2, sticky="ew")
            if not show_channel_header:
                for widget in (parameter_header, a_header, b_header):
                    widget.grid_forget()
            data_row = 0
            for idx, timing_name in enumerate(timing_names, start=first_data_row):
                timing = next(
                    (t for t in TIMINGS if t["name"].lower() == timing_name.lower() and t["Category"] == section_name),
                    None
                )
                if not timing:
                    continue
                bg_color = self._shade_row(
                    tab_name,
                    self.row_band(band_offset, uniform_header, data_row),
                )
                data_row += 1
                self._row_fill(content_frame, idx, bg_color, columns=3)
                name_label = ctk.CTkLabel(
                    content_frame,
                    text=timing["name"],
                    font=self.COMPACT_FONT,
                    height=self.ROW_HEIGHT,
                    anchor="w",
                    padx=self.ROW_PADX,
                    pady=self.ROW_PADY,
                    text_color=self.TEXT_COLOR,
                    fg_color=bg_color, bg_color=bg_color
                )
                name_label.grid(row=idx, column=0, sticky="ew")
                is_dual = is_dual_timing(timing)
                if is_dual:
                    value_a = self._read_compact_side(timing, "a")
                    value_a_label = ctk.CTkLabel(
                        content_frame,
                        text=value_a,
                        font=self.COMPACT_FONT,
                        height=self.ROW_HEIGHT,
                        anchor="w",
                        padx=5,
                        pady=4,
                        text_color=self._value_color(timing),
                        fg_color=bg_color, bg_color=bg_color
                    )
                    value_a_label.grid(row=idx, column=1, sticky="ew")
                    value_b = self._read_compact_side(timing, "b")
                    value_b_label = ctk.CTkLabel(
                        content_frame,
                        text=value_b,
                        font=self.COMPACT_FONT,
                        height=self.ROW_HEIGHT,
                        anchor="w",
                        padx=5,
                        pady=4,
                        text_color=self._value_color(timing),
                        fg_color=bg_color, bg_color=bg_color
                    )
                    value_b_label.grid(row=idx, column=2, sticky="ew")
                else:
                    value = self._read_compact_value(timing)
                    value_label = ctk.CTkLabel(
                        content_frame,
                        text=value,
                        font=self.COMPACT_FONT,
                        height=self.ROW_HEIGHT,
                        anchor="w",
                        padx=5,
                        pady=4,
                        text_color=self._value_color(timing),
                        fg_color=bg_color, bg_color=bg_color
                    )
                    value_label.grid(row=idx, column=1, sticky="ew")
                    self._register_live_value(timing, value_label)
                    empty_label = ctk.CTkLabel(
                        content_frame,
                        text="",
                        font=self.COMPACT_FONT,
                        height=self.ROW_HEIGHT,
                        anchor="w",
                        padx=5,
                        pady=4,
                        fg_color=bg_color, bg_color=bg_color
                    )
                    # Past the spacer column too. _align_dual_columns adds
                    # one beyond the three to hold the slack, and covering
                    # only column 2 left this row's band stopping 25px short
                    # of the half -- a notch in the shading right where the
                    # two halves meet, on every single-value row in a dual
                    # section. CR is the one on Timings.
                    empty_label.grid(row=idx, column=2,
                                     columnspan=max(1, self.ROW_FILL_SPAN - 2),
                                     sticky="nsew")
                if idx < len(timing_names):
                    content_frame.grid_rowconfigure(idx, weight=0, minsize=4 + int(extra_pady))
            self._register_section_body(
                tab_name, row, content_frame, first_data_row, data_row
            )
        else:
            # A section with no dual row still shares a tab with sections that
            # have one, and those lay their columns out 4:1:1. Sized from its
            # own name width instead, this one put its values hundreds of
            # pixels left of the section above -- the comp block against the
            # DFE taps on Skew. Matching the weights and joining the alignment
            # pass puts every value on the tab at one x.
            #
            # A tab with no dual row anywhere keeps the tighter layout: values
            # start just after the name gutter with a trailing spacer taking
            # the slack, because letting the value column expand there leaves
            # the label and its value at opposite edges of a full-width tab.
            if self._tab_has_dual_section(tab_name):
                content_frame.grid_columnconfigure(0, weight=4)
                content_frame.grid_columnconfigure(1, weight=1)
                content_frame.grid_columnconfigure(2, weight=1)
            else:
                content_frame.grid_columnconfigure(
                    0, weight=0, minsize=self.NAME_MINSIZE)
                content_frame.grid_columnconfigure(
                    1, weight=0, minsize=self.VALUE_MINSIZE)
                content_frame.grid_columnconfigure(2, weight=1)
            # Registered either way. Left out, a section sized its name column
            # from its own longest name, so Misc -- which has no dual row at
            # all -- put every section's values at a different x.
            self._dual_content_frames.setdefault(
                (tab_name, id(parent)), []).append(content_frame)
            data_row = 0
            for idx, timing_name in enumerate(timing_names, start=0):
                timing = next(
                    (t for t in TIMINGS if t["name"].lower() == timing_name.lower() and t["Category"] == section_name),
                    None
                )
                if not timing:
                    continue
                # Counted separately from idx: a name with no matching row is
                # skipped above, and banding on idx would then double a shade.
                bg_color = self._shade_row(
                    tab_name,
                    self.row_band(band_offset, uniform_header, data_row),
                )
                data_row += 1
                self._row_fill(content_frame, idx, bg_color, columns=3)
                name_label = ctk.CTkLabel(
                    content_frame,
                    text=timing["name"],
                    font=self.COMPACT_FONT,
                    height=self.ROW_HEIGHT,
                    anchor="w",
                    padx=self.ROW_PADX,
                    pady=self.ROW_PADY,
                    text_color=self.TEXT_COLOR,
                    fg_color=bg_color, bg_color=bg_color
                )
                name_label.grid(row=idx, column=0, sticky="ew")
                value = self._read_compact_value(timing)
                value_label = ctk.CTkLabel(
                    content_frame,
                    text=value,
                    font=self.COMPACT_FONT,
                    height=self.ROW_HEIGHT,
                    anchor="w",
                    justify="left",
                    padx=self.VALUE_PADX,
                    pady=self.ROW_PADY,
                    text_color=self._value_color(timing),
                    fg_color=bg_color, bg_color=bg_color
                )
                value_label.grid(row=idx, column=1, sticky="w")
                self._register_live_value(timing, value_label)
                if idx < len(timing_names) - 1:
                    content_frame.grid_rowconfigure(idx, weight=0, minsize=4 + int(extra_pady))
            self._register_section_body(tab_name, row, content_frame, 0, data_row)
        if return_frame:
            return section_frame

def is_admin():
    """Check if the current process has administrative privileges."""
    try:
        return ctypes.windll.shell32.IsUserAnAdmin()
    except Exception:
        return False

LAUNCHER_NAME = "run_viewer.py"


def launcher_path():
    """The script to hand back to a fresh interpreter.

    Not this module. It lives in a package and its imports are absolute, so
    running it as a script exits with ModuleNotFoundError before it draws
    anything -- which, from an elevation prompt, looks like the UAC dialog
    doing nothing at all. run_viewer.py at the project root is the one entry
    point that works, and it is what PyInstaller is pointed at too.
    """
    for directory in module_chain(__file__):
        candidate = os.path.join(directory, LAUNCHER_NAME)
        if os.path.exists(candidate):
            return candidate
    return None


def run_as_admin():
    """Relaunch with administrative privileges."""
    parameters = None
    if not getattr(sys, "frozen", False):
        launcher = launcher_path()
        if launcher is None:
            print(
                "Cannot elevate: %s was not found next to the package. Run it "
                "yourself from an Administrator prompt." % LAUNCHER_NAME
            )
            sys.exit(1)
        parameters = f'"{launcher}"'
    ctypes.windll.shell32.ShellExecuteW(
        None, "runas", sys.executable, parameters, None, 1
    )
    sys.exit(0)

def run():
    """Open the window, elevating first if this process is not elevated.

    A function rather than a body under __main__, because the module now
    lives inside a package: run_viewer.py at the root imports this and calls
    it, and PyInstaller is pointed at that launcher, so the built EXE and
    running from source take exactly the same path in.
    """
    if not is_admin():
        print("Admin privileges required. Relaunching with UAC prompt...")
        run_as_admin()
    root = ctk.CTk()
    TimingGUI(root)
    root.mainloop()


if __name__ == "__main__":
    run()

