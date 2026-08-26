# Changelog

## 1.0.0

First public release.

Roch Viewer began as a private tool validated on one bench and covering both
Intel and AMD desktop platforms. This is the Intel half, published on its own:
LGA1700 with DDR5 or DDR4, and LGA1851.

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
printing its neighbour's bits under its own name, DDR QCLK Ratio reported a
quarter of its own quantity, and the channel count named the DDR5
sub-channels where every tool it is read beside counts DIMM channels. If you
pulled between those two dates, pull again.

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
