# Changelog

## 1.0.1

Read on a second Intel bench, an MSI Z790MPOWER (MS-7E01) with an i5-14600KF,
which is where all three of these came from and is the point of the exercise:
each one was invisible on the board the code was written against.

**CA/CS/CK ODT Group B printed "RFU".** The ladder these rows decode is RZQ
divided by 0.5, 1, 2, 3, 4, 5 and 6, and the table carried "RFU" where the 48
ohm rung belongs: 480, 240, 120, 80, 60, RFU, 40, with every neighbour present
and only that step missing. All three Group B rows sit on that code here, so
all three reported a reserved setting where a real termination was programmed.
Group A was untouched because none of its codes lands there. The rows now read
RZQ/5 (48), matching Void Timings on both channels.

**VDD2 was blank.** The Super I/O rail map was derived on a different MSI board
and is board-specific by its own warning. On that board index 4 of the sensor
window is the DRAM rail at 1.572 V; here it is CPU VDD2 at 1.380 V, matching
HWiNFO exactly, with Vcore, CPU SA and CPU AUX matching it exactly too. Same
chip, same address, different rail -- so the map is now selected by board model
rather than assumed. Carrying the old map across unchanged would have printed
VDD2's voltage under the name DRAM.

**The Advanced window dumps.** Its new button writes every row to
`RochViewer.txt` on the desktop, laid out to the same column as the reference
tool's own dump so the two read side by side -- measured off that file rather
than guessed. The desktop is asked of Windows rather than built from the home
directory, which is wrong on any machine whose desktop is redirected.

**Interface.** Every hairline is one pixel and carries the brand red, muted so
a one-pixel line does not read heavier than it is; the dark surfaces all drop
by the same six values, so the steps between them are unchanged and only the
floor moved; the Summary's clock block gains the zebra shading the tables
below it have; the window title is plain text rather than red, leaving the red
to mark what is selected. The unit is spelled `MHz` throughout -- it was
`Mhz` in fifteen places and `MHz` in twenty-three -- Gear Mode reads `2`
rather than `Gear Mode 2` beside a label that already says it, the processor
loses its `(R)` and `(TM)`, and the board revision moves out of the board name
into its own System Info row.

**Housekeeping.** A hardcode and eight dead symbols. The SMBIOS-only channel
count assumed which `Physical Memory N` tags were channel A and which channel
B; it reads the channel letter off the socket locator now, the same rule the
primary path uses, because no board is obliged to number its tags that way and
this one does not. The Arrow Lake memory reference was written twice, as
`33.334` in one getter and `"133.33 Mhz"` in another, and is one named
constant. Removed: `_wmi_live`, `driver_available`, `TEMPERATURE_BLOCK_START`,
`DFE_TAP_FORMULA`, four unused bank-group constants and a duplicated
`CA_ODT_FORMULA`.

**VTT is gone on LGA 1700 DDR5.** It is a Skylake-era board rail. The DDR4 row
list already said no LGA 1700 VRM or Super I/O channel reports one, which is a
fact about the socket rather than about DDR4, so the DDR5 list was simply
missing it.


## 1.0.0

First public release.

Roch Viewer began as a private tool validated on one bench and covering both
Intel and AMD desktop platforms. This is the Intel half, published on its own:
LGA1700 with DDR5 or DDR4, and LGA1851.

Elevation relaunches through the windowed interpreter, so accepting the
administrator prompt no longer leaves a console window sitting behind the
viewer. Start from an already-elevated prompt when you want that console:
there is no elevation step to pass through, so the process keeps it.

Fixed before release, but after this repository was first pushed: the driver,
the icon and the elevation relaunch were each looked for "beside my own
module", which was the project root until the modules moved into packages.
None of them raised afterwards. The driver was never found when running from
source, so every register-backed reading was N/A while the window opened and
looked healthy; the icon fell back to Tk's default in the taskbar, while the
EXE's own file icon stayed correct; and elevation relaunched a module that
cannot run as a script, so the UAC prompt appeared and nothing followed. If
you cloned this repository before 2026-08-25, pull again.

Corrected on 2026-08-26, on an LGA1851 bench, and worth naming because these
changed readings rather than adding them. Every memory clock read half its
real value in Gear 4, and the gear cross-check reported Gear 2 against a board
in Gear 4 -- it read the two flags Raptor Lake uses, where this platform
states the gear with one. tWRPDEN, tCKCKEH, tSR and tXSDLL read nothing at
all: each spans or sits above the 32-bit boundary of a register that was being
read four bytes at a time, so no bit position could reach them. tXPDLL was
printing its neighbour's bits under its own name, and DDR QCLK Ratio reported
a quarter of its own quantity. If you pulled between those two dates, pull
again.

The channel count is decided per platform, which took two boards to establish.
Two DDR5 systems with two modules each, and the same reference tools report
different numbers: a Z790 APEX reads Quad, a Z890 TACHYON reads Dual. Both
descriptions are true of both boards -- two DIMM channels, four sub-channels --
so the row follows the convention of the tools it is read beside rather than
one rule for DDR5. A single rule in either direction was wrong for one of
them.

The AMD backend is here too, following ZenStates-Core and ZenTimings by
irusanov. That was the reason this could not ship before: those are GPL-3.0,
this was not, and clean-room provenance could not be demonstrated for a
proprietary release. Under GPL-3.0 the question changes -- building on GPL-3.0
work under GPL-3.0 is what the licence is for -- and what it asks in return is
attribution, which the files that follow that work now carry.

What is still absent are the write-capable SMU diagnostics that went with it.
They issue fixed command tuples to the AMD SMU mailbox, were gated to one
exact CPU, and are bench instruments; no licence question arises, a viewer
simply has no use for them.

### What it reads

- Primary, secondary and tertiary timings, refresh, command and power-down
  groups, both memory controllers in parallel columns.
- The electrical picture: RTT, ODT, RON, VREF levels, per-group drive
  strengths and slew compensation, ODT latencies, DFE taps.
- Per-channel read latencies, controller feature switches, ECS state,
  preambles, and the DDR5 mode registers.
- Module identity from the SPD5 hub, including on boards with SPD Write
  Disable armed.
- Live telemetry with minimum, maximum and average per reading: clocks,
  temperatures including the die's own, package and core power from the RAPL
  counters in MCHBAR, board rails, each DIMM's PMIC, the graphics card, and
  the count of hardware errors Windows has logged.

### Notes

- The low-level driver is not distributed with this project. See
  `THIRD_PARTY_NOTICES.txt` and the README's Prerequisites.
- Validated on an ASUS ROG MAXIMUS Z790 APEX with an i9-14900KS at DDR5-8000
  Gear 2, cross-checked against CPU-Z, HWiNFO, VoidTimings, ASRock Timing
  Configurator and MemTweakIt.
- Per-core temperatures and VR current are not shown. Both need RDMSR, which
  the driver this project uses does not offer; the code says so where the
  question comes up rather than approximating them.
