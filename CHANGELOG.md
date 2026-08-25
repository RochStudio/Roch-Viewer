# Changelog

## 1.0.0

First public release.

Roch Viewer began as a private tool validated on one bench and covering both
Intel and AMD desktop platforms. This is the Intel half, published on its own:
LGA1700 with DDR5 or DDR4, and LGA1851.

The AMD backend is not here and is not planned for this repository. Its
electrical-value feature was built with GPL-3.0 reference implementations
consulted as behavioural references, and formal clean-room provenance for it
could not be demonstrated, so it stays out rather than shipping under a
licence it may not be entitled to. The write-capable SMU diagnostics that went
with it are out for the same reason and one more: they were bench instruments,
gated to one exact CPU, and nothing in a viewer needs them.

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
