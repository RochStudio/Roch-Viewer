"""One searchable window holding every row the tabs show.

The tabs are organised for reading a timing set at a glance, which is the
wrong shape when you know the name of the one field you want. This window is
the other half: everything in one list, filtered as you type.

Rows are built once and hidden rather than destroyed when the filter changes.
A keystroke that rebuilt three hundred label pairs would be visibly slow, and
the widgets are identical between filters anyway -- only which of them are
gridded changes.

Each row is a frame with its own two-column grid rather than two labels in a
grid shared by the whole list. A shared grid sizes each column to its widest
cell anywhere in the list, so the motherboard string -- 378px of "ASUSTeK
COMPUTER INC. ROG MAXIMUS Z790 APEX (Rev 1.xx)" -- set the width of the value
column for all 235 rows and left the name column 5px to live in. Per-row
frames keep a long value's cost to its own row, and give the band something
that spans the full width to paint on.
"""

import customtkinter as ctk
from tkinter import font as tkfont

REFRESH_MS = 1000

ROW_HEIGHT = 19

# Each channel column, sized to the widest paired reading the tabs produce:
# 126px of "120 ns / 98 ns" on the tRFC pair, with "RZQ/6 (40)" close behind.
# It was 58, which reserved less than half of that, so two RTT values ran into
# each other with four pixels between them.
CHANNEL_WIDTH = 130

# Sized to the widest row the tabs actually produce rather than to the widest
# name plus the widest value, which never share a row: the longest names are
# on Misc against Enabled/Disabled, and the longest values are on System Info
# against names like OS and CPU. Every row still fits at 490, so this keeps
# a little slack for a board whose strings run longer than this one's.
WINDOW_SIZE = "520x800"

# The width is fixed, so the space a value gets is known and a long one can be
# wrapped instead of left to crowd its name. A single-channel row spans both
# channel columns, so it wraps at their combined width.
VALUE_WRAP = 2 * CHANNEL_WIDTH


def measuring_font_size(font):
    """The size to measure a value's width at, in pixels.

    Negative is how Tk is told pixels rather than points, and pixels is what
    customtkinter renders a label in. A positive size would be points, which
    Tk multiplies by the display's scaling factor -- 1.333 on this bench --
    so every value measured about a third wider than it draws. That is what
    made the board model claim a second line it did not use: 288 measured
    against a 260 limit, then rendered on one line at 224, leaving a
    double-height band half empty behind it.
    """
    try:
        return -abs(int(font[1]))
    except (TypeError, ValueError, IndexError, KeyError):
        return None


class AdvancedWindow(ctk.CTkToplevel):
    """Every row from the requested tabs, searchable and refreshing live."""

    def __init__(self, master, theme, entries, on_close=None, icon_path=None,
                 refresh_ms=REFRESH_MS, channel_labels=("A1", "B1")):
        super().__init__(master)
        # entries: [(tab, category, name, read())], where read returns either
        # one displayed value or an (A, B) pair for a row that reads both
        # channels. A pair gets its own column under the channel headings.
        self._entries = list(entries)
        self._channel_labels = tuple(channel_labels)
        self._theme = theme
        self._on_close = on_close
        self._refresh_ms = refresh_ms
        self._after_id = None
        self._rows = []
        self._headings = []
        self._filter = ""
        # Used to decide whether a value needs a second line. Built from the
        # theme's font rather than assumed, so a font change cannot leave the
        # measurement describing a different one.
        try:
            self._value_font = tkfont.Font(
                family=theme["font"][0],
                size=measuring_font_size(theme["font"]),
            )
        except Exception:
            self._value_font = None

        self.title("Advanced")
        self.configure(fg_color=theme["bg"])
        # A CTkToplevel writes its own icon shortly after it is created, so
        # setting ours has to happen after that, or the default one wins.
        if icon_path:
            self.after(300, lambda: self._set_icon(icon_path))
        self.geometry(WINDOW_SIZE)
        # Width fixed, height not: the list is long and worth resizing
        # vertically, while the columns are laid out against a known width.
        self.resizable(False, True)
        self.protocol("WM_DELETE_WINDOW", self.close)

        header = ctk.CTkFrame(self, corner_radius=0, fg_color="transparent")
        header.pack(fill="x", padx=6, pady=(6, 4))
        ctk.CTkLabel(
            header, text="Search:", font=theme["font"],
            text_color=theme["text"], anchor="w", width=54,
        ).pack(side="left")
        self._search = ctk.CTkEntry(
            header, font=theme["font"], text_color=theme["text"],
            fg_color=theme["header_bg"], border_width=0, height=24,
        )
        self._search.pack(side="left", fill="x", expand=True)
        self._search.bind("<KeyRelease>", self._filter_changed)
        # Escape clears rather than closing: the window is meant to stay open
        # while you look several things up in a row.
        self._search.bind("<Escape>", self._clear_search)

        self._count = ctk.CTkLabel(
            self, text="", font=theme["font"], text_color=theme["muted"],
            anchor="w",
        )
        self._count.pack(fill="x", padx=8, pady=(0, 2))

        self._body = ctk.CTkScrollableFrame(
            self, corner_radius=0, fg_color=theme["bg"]
        )
        self._body.pack(fill="both", expand=True, padx=4, pady=(0, 6))
        self._body.grid_columnconfigure(0, weight=1)

        self._build()
        self._apply_filter()
        self._search.focus()
        self.after(50, self._poll)

    # -- building -------------------------------------------------------
    def _build(self):
        """One label pair per entry, plus a heading wherever the group changes."""
        theme = self._theme
        group = None
        # Which sections have a row that reads two channels. Worked out up
        # front because a heading is built before the rows under it, and a
        # section of single-channel rows carrying A1/B1 labels names two
        # columns that nothing beneath it fills.
        paired_groups = set()
        for tab, category, name, read in self._entries:
            try:
                if isinstance(read(), tuple):
                    paired_groups.add((tab, category))
            except Exception:
                continue
        for tab, category, name, read in self._entries:
            # Whether a row reads two channels is fixed by the row, so it is
            # settled once here rather than re-decided on every refresh.
            try:
                paired = isinstance(read(), tuple)
            except Exception:
                paired = False
            if (tab, category) != group:
                group = (tab, category)
                title = category if category else tab
                # No colour of its own: a heading is a row of the table and
                # takes the band its position calls for, which is what the
                # Timings and Skew tabs do. Given one, the alternation would
                # visibly restart at every section.
                heading = ctk.CTkFrame(self._body, corner_radius=0,
                                       fg_color="transparent")
                heading.grid_columnconfigure(0, weight=1)
                heading.grid_columnconfigure(1, minsize=CHANNEL_WIDTH)
                heading.grid_columnconfigure(2, minsize=CHANNEL_WIDTH)
                ctk.CTkLabel(
                    heading, text=f"{tab} — {title}".upper(),
                    font=theme["bold"], text_color=theme["value"],
                    anchor="w", fg_color="transparent", height=ROW_HEIGHT,
                ).grid(row=0, column=0, sticky="w", padx=(6, 4))
                # The channel names ride on the section heading rather than
                # taking a row of their own, which would break the band and
                # cost a line per section. Only where the section has a row
                # to put under them.
                if group in paired_groups:
                    for column, text in enumerate(self._channel_labels,
                                                  start=1):
                        ctk.CTkLabel(
                            heading, text=text, font=theme["bold"],
                            text_color=theme["muted"], anchor="e",
                            fg_color="transparent", height=ROW_HEIGHT,
                        ).grid(row=0, column=column, sticky="e",
                               padx=(4, 8 if column == 2 else 4))
                self._headings.append((heading, group))

            # The frame takes its height from its contents so a wrapped value
            # gets a second line instead of being clipped to one. The name
            # label carries the row height, which keeps every other row at the
            # same pitch.
            frame = ctk.CTkFrame(self._body, corner_radius=0,
                                 fg_color="transparent")
            # Weighted so a value wider than the space left over takes it from
            # the name, in that row alone, instead of from every row.
            frame.grid_columnconfigure(0, weight=1)
            frame.grid_columnconfigure(1, minsize=CHANNEL_WIDTH)
            frame.grid_columnconfigure(2, minsize=CHANNEL_WIDTH)
            ctk.CTkLabel(
                frame, text=name, font=theme["font"],
                text_color=theme["text"], anchor="nw", fg_color="transparent",
                height=ROW_HEIGHT,
            ).grid(row=0, column=0, sticky="nw", padx=(6, 4))
            # Two cells always, so a single-value row's value lands in the
            # same column as a channel-A reading rather than wherever its own
            # name happens to end.
            value = ctk.CTkLabel(
                frame, text="", font=theme["font"],
                text_color=theme["value"], anchor="e", fg_color="transparent",
                wraplength=VALUE_WRAP, justify="right", height=ROW_HEIGHT,
            )
            # A row that reads one channel spans both columns rather than
            # leaving the second empty: the long values are all single-channel
            # -- the motherboard string, the ECS and preamble text -- and they
            # need the width the second column would otherwise hold open.
            if paired:
                value.grid(row=0, column=1, sticky="e", padx=(4, 4))
                value_b = ctk.CTkLabel(
                    frame, text="", font=theme["font"],
                    text_color=theme["value"], anchor="e",
                    fg_color="transparent", justify="right",
                    height=ROW_HEIGHT,
                )
                value_b.grid(row=0, column=2, sticky="e", padx=(4, 8))
            else:
                value.grid(row=0, column=1, columnspan=2, sticky="e",
                           padx=(4, 8))
                value_b = None
            self._rows.append({
                "group": group,
                "haystack": f"{tab} {category} {name}".lower(),
                "frame": frame,
                "value": value,
                "value_b": value_b,
                "read": read,
                "shown": (None, None),
            })

    # -- filtering ------------------------------------------------------
    def _clear_search(self, _event=None):
        self._search.delete(0, "end")
        self._filter_changed()

    def _filter_changed(self, _event=None):
        text = self._search.get().strip().lower()
        if text == self._filter:
            return
        self._filter = text
        self._apply_filter()

    def _band(self, line):
        """The background for one line of the list, headings included."""
        return self._theme["band"] if line % 2 else self._theme["bg"]

    def _matches(self, row):
        # Every word must appear somewhere in the row, so "skew vref" narrows
        # rather than widening the way a single substring would. Folded here
        # as well as on the way in: the haystack is built lowercase, and a
        # filter that skipped the fold would silently match nothing.
        return all(word in row["haystack"]
                   for word in self._filter.lower().split())

    def _apply_filter(self):
        visible = [row for row in self._rows if self._matches(row)]
        wanted = {row["group"] for row in visible}

        shown = set(id(row) for row in visible)
        for heading, group in self._headings:
            if group not in wanted:
                heading.grid_remove()
        for row in self._rows:
            if id(row) not in shown:
                row["frame"].grid_remove()

        # Re-gridded from the top each time so a filtered list has no gaps
        # where the hidden rows used to be. The band runs off the line number,
        # unbroken from the top of the list: counted per section instead, it
        # restarted at every heading, and two sections in a row could put the
        # same shade on the last row of one and the first row of the next.
        line = 0
        group = None
        heading_for = {group: heading for heading, group in self._headings}
        for row in visible:
            if row["group"] != group:
                group = row["group"]
                heading = heading_for.get(group)
                if heading is not None:
                    heading.configure(fg_color=self._band(line))
                    heading.grid(row=line, column=0, columnspan=2,
                                 sticky="ew", padx=0, pady=0)
                    line += 1
            row["frame"].configure(fg_color=self._band(line))
            row["frame"].grid(row=line, column=0, sticky="ew")
            line += 1

        self._visible = visible
        total = len(self._rows)
        if len(visible) == total:
            self._count.configure(text=f"{total} rows")
        else:
            self._count.configure(text=f"{len(visible)} of {total} rows")
        self._refresh_values()

    # -- refreshing -----------------------------------------------------
    def _refresh_values(self):
        """Read only what is on screen; a filtered list costs a filtered read."""
        for row in getattr(self, "_visible", ()):
            try:
                reading = row["read"]()
            except Exception:
                reading = "N/A"
            # A pair means the row reads two channels; anything else is one
            # value, which sits in the channel-A column so it lines up with
            # the readings above and below it.
            if isinstance(reading, tuple):
                # Both columns always, even where the channels agree. Blanking
                # the matching side saved width but emptied B1 on almost every
                # row, since most settings are the same on both modules -- and
                # an empty column reads as no reading rather than as agreement.
                left, right = ("" if part is None else str(part)
                               for part in reading)
            else:
                left = "" if reading is None else str(reading)
                right = ""
            if (left, right) == row["shown"]:
                continue
            row["shown"] = (left, right)
            try:
                row["value"].configure(
                    text=left, height=self._value_height(left))
                if row["value_b"] is not None:
                    row["value_b"].configure(text=right)
            except Exception:
                # The widget went away while the window was closing.
                continue

    def _value_height(self, text):
        """One row high, or two when the value has to wrap to fit.

        Measured rather than guessed from the character count: the row names
        are monospaced but the values are not all the same width, and a value
        that wrapped in a label kept at one line would lose its second half
        with nothing on screen to say so.
        """
        if self._value_font is None:
            return ROW_HEIGHT
        try:
            if self._value_font.measure(text) > VALUE_WRAP:
                return ROW_HEIGHT * 2
        except Exception:
            pass
        return ROW_HEIGHT

    def _poll(self):
        if not self.winfo_exists():
            return
        self._refresh_values()
        self._after_id = self.after(self._refresh_ms, self._poll)

    # -- lifecycle ------------------------------------------------------
    def _set_icon(self, path):
        try:
            self.iconbitmap(path)
        except Exception:
            pass

    def close(self):
        if self._after_id is not None:
            try:
                self.after_cancel(self._after_id)
            except Exception:
                pass
            self._after_id = None
        if self._on_close is not None:
            self._on_close()
        self.destroy()
