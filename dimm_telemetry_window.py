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

"""The Telemetry pop-out: every live reading, with statistics.

Separate from the tabs because it behaves differently. The tabs read settings,
which do not move; this polls and keeps what each reading did over time, which
is the only way a rail that sags under load shows itself. Everything that
moves lives here: the board and CPU sensors that used to be the Sensors tab,
and each DIMM's own PMIC.

Reading a PMIC channel costs one ADC channel select -- see
:mod:`ddr5_telemetry` -- so the poll is deliberately unhurried and stops the
moment the window closes.
"""

from __future__ import annotations

import re
import threading

import customtkinter as ctk

POLL_MS = 1000

# Tall enough for two panels; a four-DIMM board scrolls.
WINDOW_SIZE = "600x800"

# (key, label, unit, decimals). Order is the order they are shown.
PARAMETERS = (
    ("hub_temp_c", "SPD Hub Temp", "°C", 2),
    ("vdd", "VDD (SWA)", "V", 3),
    ("vddq", "VDDQ (SWB)", "V", 3),
    ("vpp", "VPP (SWC)", "V", 3),
    ("vin_bulk", "VIN Bulk", "V", 3),
    ("vout_1v8", "VOUT 1.8V", "V", 3),
    ("vout_1v0", "VOUT 1.0V", "V", 3),
    # The sum only. The three per-rail figures it is made of were shown
    # above it and said nothing the total does not: VDD carries almost all
    # of a DDR5 module's draw, so the split read as the total, a small
    # number and a rounding error on every board tried.
    ("power_w", "Total Power", "W", 3),
)

# Millivolt readings are shown in volts; the rest are already in their unit.
MILLIVOLT_KEYS = frozenset(
    {"vdd", "vddq", "vpp", "vin_bulk", "vout_1v8", "vout_1v0"}
)

COLUMNS = ("Parameter", "Current", "Min", "Max", "Average")

# Groups that belong below the per-DIMM panels rather than above them. The
# sensor tables are built before any DIMM has answered, so a group meant for
# the foot of the window has to be sunk once the panels exist.
#
# Errors is there because it is not a reading of anything: it is the counter
# you check after a run rather than watch during one, and putting it between
# the voltages and the modules interrupted two things that belong together.
# Graphics is here because the card's sensors are a separate machine from
# the one the rest of the window describes, and reading past them to reach
# the modules put the memory tool's own subject last. Errors stays after it.
TRAILING_GROUPS = ("Graphics", "Errors")

# A row with children carries one of these ahead of its name, and folds them
# away until it is clicked. Sixteen logical processors is a useful thing to be
# able to open and a poor thing to have to scroll past to reach the
# temperatures, so they start closed.
COLLAPSED_MARK = "▸ "
EXPANDED_MARK = "▾ "
# Children are indented under the parent rather than marked, which keeps the
# Parameter column readable at a glance.
CHILD_INDENT = "    "

# A displayed reading is text: "1.244 V", "38.7 / 300.0 W", "42.1 °C", "Off".
# The statistics need the number, and the first one is always the reading --
# where a second follows it is the configured limit, which does not move.
_NUMBER = re.compile(r"-?\d+(?:\.\d+)?")
_UNIT = re.compile(r"[-\d.\s/]+(.*)$")


def parse_reading(text):
    """Return ``(value, unit, decimals)`` for a displayed reading.

    ``(None, "", 0)`` when there is no number in it, which is how a row
    reading "Off" or an em dash keeps its place without inventing statistics.
    """
    text = str(text or "").strip()
    match = _NUMBER.search(text)
    if not match:
        return None, "", 0
    number = match.group(0)
    _, _, fraction = number.partition(".")
    unit_match = _UNIT.match(text)
    unit = (unit_match.group(1).strip() if unit_match else "")
    if any(character.isdigit() for character in unit):
        # A second number followed: the unit is what trails the last one.
        parts = unit.split()
        unit = parts[-1] if parts else ""
    return float(number), unit, len(fraction)


# Rows sampled between ticks as well as on them.
#
# The window redraws once a second, which is often enough to read and not
# often enough to see what a die temperature did in between: against HWiNFO
# polling faster, our Core Max maximum came out 58 where its was 61 over the
# same run, while the minimum and the average agreed exactly. A maximum that
# misses the peaks is the wrong number on the one row whose peaks are the
# point.
#
# Only rows that are cheap to read belong here. Core Max is a single MCHBAR
# word, microseconds, so it can be read on the UI thread without the worker
# the full tick needs.
FAST_SAMPLE_LABELS = frozenset({"Core Max"})
FAST_SAMPLE_MS = 200


class Statistic:
    """Min, max and running mean for one parameter of one DIMM."""

    __slots__ = ("current", "minimum", "maximum", "total", "count")

    def __init__(self):
        self.reset()

    def reset(self):
        self.current = None
        self.minimum = None
        self.maximum = None
        self.total = 0.0
        self.count = 0

    def add(self, value):
        if value is None:
            return
        value = float(value)
        self.current = value
        self.minimum = value if self.minimum is None else min(self.minimum, value)
        self.maximum = value if self.maximum is None else max(self.maximum, value)
        self.total += value
        self.count += 1

    @property
    def average(self):
        return self.total / self.count if self.count else None


def reading_value(entry, key):
    """Pull one parameter out of a telemetry entry, in display units."""
    if key not in entry:
        return None
    value = entry[key]
    return value / 1000.0 if key in MILLIVOLT_KEYS else value


def format_value(value, unit, decimals):
    return "—" if value is None else "%.*f %s" % (decimals, value, unit)


def format_elapsed(seconds):
    seconds = max(0, int(seconds))
    return "%02d:%02d:%02d" % (
        seconds // 3600, (seconds % 3600) // 60, seconds % 60
    )


def panel_title(index, entry, module):
    """The heading over one DIMM's table: which module, and which PMIC."""
    slot = (module or {}).get("slot") or (entry.get("channel", "?").upper())
    return "DIMM %d  |  %s  |  PMIC 0x%02X" % (
        index, slot, entry.get("pmic_address", 0)
    )


def panel_identity(entry, module):
    """Return the identity lines under a heading, as label/value pairs.

    Two different makers appear here and the labels keep them apart: the
    module vendor is whose name is on the stick, while the DRAM manufacturer
    is who made the chips on it. They are rarely the same company -- G.Skill
    sells a module carrying SK hynix silicon.
    """
    from dimm_inventory import split_ic

    module = module or {}
    capacity = module.get("capacity_gb")
    maker, die = split_ic(module.get("ic"))
    pmic = entry.get("vendor") or "—"
    if entry.get("revision"):
        pmic = "%s rev %s" % (pmic, entry["revision"])

    # Left reads down the chips themselves; right reads down the stick they
    # are on and what powers it. Both makers are named for what they made --
    # the ICs and the module -- because "manufacturer" alone fits either.
    left = [
        ("IC Manufacturer", maker),
        ("DRAM Die", die),
        ("Rank", module.get("rank_numeric") or "—"),
    ]
    right = [
        ("DRAM Part Number", module.get("part_number") or "—"),
        ("Module Manufacturer", module.get("spd_vendor")
         or module.get("module_manufacturer") or "—"),
        ("Capacity", "%d MB" % (capacity * 1024) if capacity else "—"),
        ("PMIC", pmic),
    ]
    # The two sides are drawn in pairs, so the shorter one is padded rather
    # than cutting the longer one short.
    while len(left) < len(right):
        left.append(("", ""))
    while len(right) < len(left):
        right.append(("", ""))
    return left, right


class DimmTelemetryWindow(ctk.CTkToplevel):
    """One window, one panel per populated DIMM, polling while it is open."""

    def __init__(self, master, theme, read_telemetry, read_modules,
                 on_close=None, poll_ms=POLL_MS, auto_open=False,
                 on_auto_open=None, sensor_groups=(), icon_path=None):
        super().__init__(master)
        # [(group title, [(label, read() -> displayed text)])]
        self._sensor_groups = list(sensor_groups)
        self._theme = theme
        self._read_telemetry = read_telemetry
        self._read_modules = read_modules
        self._on_close = on_close
        self._poll_ms = poll_ms
        self._after_id = None
        self._elapsed = 0
        self._poll_failed = False
        # True while a worker is reading, so an overrunning tick is
        # skipped rather than stacked on the one before it.
        self._reading = False
        self._stats = {}
        self._cells = {}
        self._panels_built = False
        # {group title: its frame}, so a trailing group can be moved later.
        self._group_panels = {}
        # {parent label: [[widgets of one child row], ...]} and which parents
        # are currently open. Children keep polling while folded away, so
        # their minimum and maximum cover the whole run rather than starting
        # from whenever the row was first opened.
        self._children = {}
        self._expanded = {}

        self._on_auto_open = on_auto_open

        self.title("Telemetry")
        self.configure(fg_color=theme["bg"])
        # A CTkToplevel writes its own icon shortly after it is created, so
        # setting ours has to happen after that, or the default one wins.
        if icon_path:
            self.after(300, lambda: self._set_icon(icon_path))
        self.geometry(WINDOW_SIZE)
        self.protocol("WM_DELETE_WINDOW", self.close)

        self._body = ctk.CTkScrollableFrame(
            self, corner_radius=0, fg_color=theme["bg"]
        )
        self._body.pack(fill="both", expand=True, padx=4, pady=(4, 0))

        footer = ctk.CTkFrame(self, corner_radius=0, fg_color="transparent",
                              height=28)
        footer.pack(fill="x", padx=6, pady=4)
        self._running_label = ctk.CTkLabel(
            footer, text="Running: 00:00:00", font=theme["font"],
            text_color=theme["text"], anchor="w",
        )
        self._running_label.pack(side="left")
        ctk.CTkButton(
            footer, text="Reset Stats", command=self.reset_statistics,
            width=90, height=22, font=theme["bold"],
            fg_color=theme["button"], hover_color=theme["button_hover"],
            text_color=theme["text"],
        ).pack(side="right")

        # Opening this window costs an SMBus poll every second, so whether it
        # opens with the app is remembered rather than assumed.
        self._auto_open = ctk.BooleanVar(value=bool(auto_open))
        ctk.CTkSwitch(
            footer, text="Auto-open", variable=self._auto_open,
            command=self._auto_open_changed, font=theme["font"],
            text_color=theme["text"], width=90, height=20,
            switch_width=32, switch_height=16,
            progress_color=theme["button_hover"],
        ).pack(side="right", padx=(0, 10))

        self._build_sensor_groups()
        # Rows whose statistics are fed between ticks, and the keys to skip
        # when the tick comes round: fed from both, the average would count
        # the same row five times a second and once a second.
        self._fast_rows = [
            (("sensor", title, label), read)
            for title, group in self._sensor_groups
            for label, read, _parent in group
            if label in FAST_SAMPLE_LABELS
        ]
        self._fast_keys = {key for key, _read in self._fast_rows}
        self._fast_after_id = None
        self.after(50, self._poll)
        if self._fast_rows:
            self._fast_after_id = self.after(FAST_SAMPLE_MS, self._fast_sample)

    # -- building -------------------------------------------------------
    def _band(self, data_row):
        """The background for one data row, alternating from an unshaded one."""
        if data_row % 2:
            return self._theme.get("band", "transparent")
        return "transparent"

    def _band_row(self, table, row):
        """Carry a row's tint past its last value, to the end of the table.

        The cells only cover the text they hold, so tinting them alone leaves
        the band stopping at each one. A label spanning the row fills the gaps
        between them, and it is created before the cells so they sit on top.

        Made for every row rather than only the tinted ones, and coloured
        later. A row that can be folded away has to be able to take its filler
        with it, and one that was never given a filler cannot: sixteen hidden
        rows left sixteen fillers still gridded, holding open a block of empty
        table where the per-processor clocks had been.
        """
        filler = ctk.CTkLabel(
            table, text="", fg_color="transparent", corner_radius=0,
        )
        filler.grid(row=row, column=0, columnspan=len(COLUMNS), sticky="nsew")
        return filler

    def _restripe(self, records):
        """Colour one table's bands by what is currently visible.

        Banding by the row's position in the table breaks as soon as rows can
        be hidden: the alternation counts rows nobody can see, so closing a
        group leaves two shaded rows adjacent. Counting only the visible ones
        keeps the stripes alternating whatever is open.
        """
        visible = 0
        for record in records:
            parent = record["parent"]
            if parent and not self._expanded.get(parent):
                continue
            background = self._band(visible)
            record["filler"].configure(fg_color=background)
            for widget in record["cells"]:
                widget.configure(fg_color=background, bg_color=background)
            visible += 1

    def _stat_table(self, parent, heading, rows, remember=None):
        """Draw one titled table and return its cells, keyed by row key."""
        theme = self._theme
        panel = ctk.CTkFrame(parent, corner_radius=0, fg_color="transparent")
        panel.pack(fill="x", pady=(0, 10))
        if remember is not None:
            self._group_panels[remember] = panel
        ctk.CTkLabel(
            panel, text=heading, font=theme["bold"], anchor="w", padx=6, pady=3,
            text_color=theme["text"], fg_color=theme["header_bg"],
        ).pack(fill="x")

        table = ctk.CTkFrame(panel, corner_radius=0, fg_color="transparent")
        table.pack(fill="x")
        table.grid_columnconfigure(0, weight=0, minsize=150)
        for column in range(1, len(COLUMNS)):
            table.grid_columnconfigure(column, weight=1, uniform="stat")
        for column, name in enumerate(COLUMNS):
            ctk.CTkLabel(
                table, text=name, font=theme["bold"], anchor="w", padx=6,
                text_color=theme["text"], fg_color=theme["header_bg"],
            ).grid(row=0, column=column, sticky="ew")

        cells = {}
        # Which labels have children, so a parent knows to draw a toggle and
        # a child knows to start hidden.
        parents = {parent for _k, _l, parent in rows if parent}
        records = []
        for row, (key, label, parent) in enumerate(rows, 1):
            filler = self._band_row(table, row)
            text = CHILD_INDENT + label if parent else label
            if label in parents:
                text = COLLAPSED_MARK + label
            name = ctk.CTkLabel(
                table, text=text, font=theme["font"], anchor="w", padx=6,
                text_color=theme["text"],
            )
            name.grid(row=row, column=0, sticky="ew")
            record = {"filler": filler, "cells": [name], "parent": parent}
            for column in range(1, len(COLUMNS)):
                cell = ctk.CTkLabel(
                    table, text="—", font=theme["font"], anchor="w", padx=6,
                    text_color=theme["value"],
                )
                cell.grid(row=row, column=column, sticky="ew")
                cells[(key, column)] = cell
                record["cells"].append(cell)
            records.append(record)
            if parent:
                self._children.setdefault(parent, []).append(record)
                self._hide(record)
            elif label in parents:
                self._bind_toggle(name, label, records)
        self._restripe(records)
        return cells

    @staticmethod
    def _hide(record):
        record["filler"].grid_remove()
        for widget in record["cells"]:
            widget.grid_remove()

    @staticmethod
    def _show(record):
        record["filler"].grid()
        for widget in record["cells"]:
            widget.grid()

    def _bind_toggle(self, label_widget, name, records):
        """Make a parent row open and close the rows folded under it."""
        def toggle(_event=None):
            expanded = not self._expanded.get(name, False)
            self._expanded[name] = expanded
            for record in self._children.get(name, ()):
                if expanded:
                    self._show(record)
                else:
                    self._hide(record)
            label_widget.configure(
                text=(EXPANDED_MARK if expanded else COLLAPSED_MARK) + name
            )
            self._restripe(records)

        label_widget.configure(cursor="hand2")
        label_widget.bind("<Button-1>", toggle)

    def _build_sensor_groups(self):
        """Draw the board and CPU sensors, one table per group."""
        self._sensor_cells = {}
        for title, readings in self._sensor_groups:
            rows = [
                (("sensor", title, label), label, parent)
                for label, _read, parent in readings
            ]
            self._sensor_cells.update(
                self._stat_table(self._body, title.upper(), rows,
                                 remember=title)
            )

    def _build_panels(self, entries, modules):
        theme = self._theme
        for index, entry in enumerate(entries):
            module = modules.get(entry.get("channel"))
            panel = ctk.CTkFrame(self._body, corner_radius=0,
                                 fg_color="transparent")
            panel.pack(fill="x", pady=(0, 10))

            heading = ctk.CTkLabel(
                panel, text=panel_title(index, entry, module),
                font=theme["bold"], anchor="w", padx=6, pady=3,
                text_color=theme["text"], fg_color=theme["header_bg"],
            )
            heading.pack(fill="x")

            identity = ctk.CTkFrame(panel, corner_radius=0,
                                    fg_color="transparent")
            identity.pack(fill="x", pady=(2, 4))
            identity.grid_columnconfigure(0, weight=0, minsize=140)
            identity.grid_columnconfigure(1, weight=1)
            identity.grid_columnconfigure(2, weight=0, minsize=165)
            identity.grid_columnconfigure(3, weight=1)
            left, right = panel_identity(entry, module)
            for row, ((left_label, left_value), (right_label, right_value)) in \
                    enumerate(zip(left, right)):
                for column, (label, value) in (
                    (0, (left_label, left_value)), (2, (right_label, right_value)),
                ):
                    ctk.CTkLabel(
                        identity, text=label, font=theme["font"], anchor="w",
                        padx=6, text_color=theme["muted"],
                    ).grid(row=row, column=column, sticky="w")
                    ctk.CTkLabel(
                        identity, text=value, font=theme["font"], anchor="w",
                        padx=2, text_color=theme["text"],
                    ).grid(row=row, column=column + 1, sticky="w")

            table = ctk.CTkFrame(panel, corner_radius=0, fg_color="transparent")
            table.pack(fill="x")
            table.grid_columnconfigure(0, weight=0, minsize=150)
            for column in range(1, len(COLUMNS)):
                table.grid_columnconfigure(column, weight=1, uniform="stat")
            for column, name in enumerate(COLUMNS):
                ctk.CTkLabel(
                    table, text=name, font=theme["bold"], anchor="w", padx=6,
                    text_color=theme["text"], fg_color=theme["header_bg"],
                ).grid(row=0, column=column, sticky="ew")

            # Banded through the same two calls the sensor tables use. Nothing
            # here folds away, so every row is visible and _restripe colours
            # them in order.
            records = []
            for row, (key, label, _unit, _decimals) in enumerate(PARAMETERS, 1):
                filler = self._band_row(table, row)
                name = ctk.CTkLabel(
                    table, text=label, font=theme["font"], anchor="w", padx=6,
                    text_color=theme["text"],
                )
                name.grid(row=row, column=0, sticky="ew")
                record = {"filler": filler, "cells": [name], "parent": None}
                for column in range(1, len(COLUMNS)):
                    cell = ctk.CTkLabel(
                        table, text="—", font=theme["font"], anchor="w", padx=6,
                        text_color=theme["value"],
                    )
                    cell.grid(row=row, column=column, sticky="ew")
                    self._cells[(index, key, column)] = cell
                    record["cells"].append(cell)
                records.append(record)
            self._restripe(records)
        self._sink_trailing_groups()
        self._panels_built = True

    def _sink_trailing_groups(self):
        """Re-pack the trailing groups so they sit under the DIMM panels.

        Tk packs in call order, and these tables were drawn before any module
        had answered. Forgetting and re-packing moves a frame to the end
        without rebuilding it, so the rows keep the statistics they have
        already collected.
        """
        for title in TRAILING_GROUPS:
            panel = self._group_panels.get(title)
            if panel is None:
                continue
            try:
                panel.pack_forget()
                panel.pack(fill="x", pady=(0, 10))
            except Exception:
                continue

    # -- polling --------------------------------------------------------
    def _read_sensors(self):
        """Read every sensor row. Off the UI thread; touches no widget."""
        readings = []
        for title, group in self._sensor_groups:
            for label, read, _parent in group:
                try:
                    text = read()
                except Exception:
                    text = "—"
                readings.append((("sensor", title, label), text))
        return readings

    def _apply_sensors(self, readings):
        """Write the sensor readings into their cells. UI thread only.

        The Current column keeps the row's own text, so a reading that pairs a
        value with its limit -- "38.7 / 300.0 W" -- still reads the way it
        does everywhere else, while the statistics track the first number,
        which is the part that moves.
        """
        for key, text in readings:
            value, unit, decimals = parse_reading(text)
            statistic = self._stats.setdefault(key, Statistic())
            # A fast-sampled row is already being fed, five times a second.
            # Adding here as well would weight this one sample like five and
            # pull the average toward whatever the tick happened to catch.
            if key not in self._fast_keys:
                statistic.add(value)
            current = self._sensor_cells.get((key, 1))
            if current is not None:
                current.configure(text=str(text))
            for column, tracked in (
                (2, statistic.minimum), (3, statistic.maximum),
                (4, statistic.average),
            ):
                cell = self._sensor_cells.get((key, column))
                if cell is not None:
                    cell.configure(text=format_value(tracked, unit, decimals))

    def _fast_sample(self):
        """Feed the between-tick statistics for rows whose peaks matter.

        On the UI thread, unlike the full tick: these rows are a register
        read each, and handing them to a worker would cost more in thread
        handoff than the read itself. Nothing here touches a widget -- the
        cells are redrawn on the next tick, with the extremes this collected.
        """
        for key, read in self._fast_rows:
            try:
                value, _unit, _decimals = parse_reading(read())
            except Exception:
                continue
            if value is not None:
                self._stats.setdefault(key, Statistic()).add(value)
        self._fast_after_id = self.after(FAST_SAMPLE_MS, self._fast_sample)

    def _poll(self):
        """Start one tick's reading, unless the last one is still going.

        The reads happen on a worker because they block: a DIMM's eight ADC
        channels need 12 ms each to settle, which is 183 ms per tick of two
        modules, and on the UI thread that is 183 ms in every second where
        the window will not scroll. Tk is not thread-safe, so nothing here
        touches a widget -- the results go back through after(0, ...) and are
        applied on the UI thread.

        A tick that overruns its interval is skipped rather than queued, the
        way the main window's refresh does it: falling behind should cost
        samples, not build a backlog against the same mutexes.
        """
        if self._reading:
            self._after_id = self.after(self._poll_ms, self._poll)
            return
        self._reading = True
        threading.Thread(target=self._read_tick, daemon=True).start()

    def _read_tick(self):
        """Worker: read everything this tick needs, then hand it back."""
        readings, entries, modules = [], [], {}
        # Any value backed by WMI needs COM initialised on this thread. The
        # rows that use it cache for the session and are normally warm before
        # this window opens, but "normally" is not a guarantee worth a silent
        # blank row.
        com_ready = False
        try:
            import pythoncom

            pythoncom.CoInitialize()
            com_ready = True
        except Exception:
            pass
        try:
            readings = self._read_sensors()
            try:
                entries = self._read_telemetry() or []
            except Exception:
                entries = []
            if entries and not self._panels_built:
                try:
                    modules = {
                        (module.get("channel") or "").lower(): module
                        for module in (self._read_modules() or [])
                    }
                except Exception:
                    modules = {}
        except Exception as exc:
            self._report_failure(exc)
        finally:
            if com_ready:
                try:
                    import pythoncom

                    pythoncom.CoUninitialize()
                except Exception:
                    pass
        try:
            self.after(0, self._apply_tick, readings, entries, modules)
        except Exception:
            # The window is gone; nothing left to update.
            self._reading = False

    def _apply_tick(self, readings, entries, modules):
        """Draw one tick's readings and book the next. UI thread only.

        The next tick is booked in a finally, so a fault here stops a reading
        rather than the window. Before that, a mismatch between two drawing
        calls raised on the first tick and the poll was simply never
        rescheduled again: the panels stood half drawn and the timer sat at
        00:00:00, which reads as "nothing to report" rather than "broken".
        """
        try:
            self._apply_sensors(readings)
            if entries and not self._panels_built:
                self._build_panels(entries, modules)

            for index, entry in enumerate(entries):
                for key, _label, unit, decimals in PARAMETERS:
                    statistic = self._stats.setdefault((index, key), Statistic())
                    statistic.add(reading_value(entry, key))
                    for column, value in (
                        (1, statistic.current), (2, statistic.minimum),
                        (3, statistic.maximum), (4, statistic.average),
                    ):
                        cell = self._cells.get((index, key, column))
                        if cell is not None:
                            cell.configure(
                                text=format_value(value, unit, decimals)
                            )

            self._elapsed += self._poll_ms / 1000.0
            self._running_label.configure(
                text="Running: %s" % format_elapsed(self._elapsed)
            )
        except Exception as exc:
            self._report_failure(exc)
        finally:
            self._reading = False
            self._after_id = self.after(self._poll_ms, self._poll)

    def _report_failure(self, exc):
        """Say what went wrong once, rather than every tick from now on."""
        if not self._poll_failed:
            self._poll_failed = True
            print(f"Telemetry poll failed: {exc!r}")

    def _set_icon(self, path):
        try:
            self.iconbitmap(path)
        except Exception as exc:
            print(f"Could not set the telemetry window icon: {exc}")

    def _auto_open_changed(self):
        if self._on_auto_open is not None:
            self._on_auto_open(bool(self._auto_open.get()))

    def reset_statistics(self):
        for statistic in self._stats.values():
            statistic.reset()
        self._elapsed = 0
        for cell in list(self._cells.values()) + list(self._sensor_cells.values()):
            cell.configure(text="—")
        self._running_label.configure(text="Running: 00:00:00")

    def close(self):
        for attribute in ("_after_id", "_fast_after_id"):
            handle = getattr(self, attribute, None)
            if handle is not None:
                try:
                    self.after_cancel(handle)
                except Exception:
                    pass
                setattr(self, attribute, None)
        if self._on_close is not None:
            self._on_close()
        self.destroy()
