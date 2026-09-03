# Roch Viewer 1.0.1

Roch Viewer is a Windows memory-controller and timing viewer for Intel and AMD
desktop platforms. It reads the memory controller, the modules, the board and the CPU
directly, and shows what they actually report — the timings the controller was
programmed with, the training results it settled on, and the sensors that move
while the machine runs.

It is a read-only viewer. It changes no setting, and nothing it shows is a
value it was told to expect.

## Supported platforms

| Platform | Memory | Timing source |
| --- | --- | --- |
| LGA1700 (Z690/Z790) | DDR5 and DDR4 | Intel MCHBAR |
| LGA1851 (Z890) | DDR5 | Intel MCHBAR |
| AM5 / Granite Ridge | DDR5 | AMD UMC, SMU and APOB |

The platform is resolved before any privileged read, and an unrecognised
machine gets a backend that reads nothing and says so, rather than pointing
MCHBAR offsets at silicon they were not written for.

The Intel side is validated on an ASUS ROG MAXIMUS Z790 APEX with an
i9-14900KS at DDR5-8000 Gear 2, cross-checked against CPU-Z, HWiNFO,
VoidTimings, ASRock Timing Configurator and MemTweakIt.

Two further benches, and the reason both are named: every defect 1.0.1 fixes
was invisible on the board the code was written against. A Gigabyte Z890
AORUS TACHYON ICE with a Core Ultra 7 270K Plus at DDR5-8800 covers LGA1851,
and an MSI Z790MPOWER with an i5-14600KF at DDR5-8200 covers a second LGA1700
board -- where the Super I/O rail map derived on the first one turned out to
name a different rail, which is why that map is now chosen by board model
rather than assumed.

The AMD side was developed and validated on an MSI B850MPOWER with a Ryzen
9850X3D, against same-boot ZenTimings, and has not been re-run since it was
brought into this repository. Treat its readings as needing confirmation
against a second tool on your own board. Its electrical values -- RTT, ODT and
the drive strengths -- come from a reverse-engineered APOB parser whose
channel attribution is verified only for the two-record geometry observed on
that bench.

The AMD backend follows ZenStates-Core and ZenTimings by irusanov, both
GPL-3.0, which is why this project is GPL-3.0 too. See
`THIRD_PARTY_NOTICES.txt`.

## Prerequisites

- Windows 10 or 11, 64-bit.
- Administrator rights. Low-level register access requires them.
- **The inpoutx64 driver**, which is not distributed here. Download
  `inpoutx64.dll` and `inpoutx64.sys` from Highresolution Enterprises
  (www.highrez.co.uk) and put both beside `RochViewer.exe`, or beside
  `run_viewer.py` to run from source. See `THIRD_PARTY_NOTICES.txt` for why they are
  not bundled.

Windows security software may warn about that driver. It is a well-known
low-level access component and is also, for the same reason, a component
attackers have abused; decide for yourself whether you want it on your
machine.

## What it shows

**Summary** — one screen with the machine's identity, the clock chain, and the
timings worth seeing at a glance, laid out in three columns with the signal
levels beside them. It is sized to hold everything it shows, so it carries no
scrollbar.

**System Info** (50 rows) — the OS and platform, the processor with its CPUID
code name and process node, the board with its revision, chipset, southbridge
and Super I/O, the clock chain, the memory with each module's SPD identity,
and the graphics card.

**Timings** (78 rows) — primary, secondary and tertiary timings, the refresh
group, command timings, the power-down group, and the bus timings that belong
to none of those. Every per-controller timing reads both memory controllers,
in two columns named for the slots that are actually populated.

**Skew** (85 rows) — the electrical picture: RTT, ODT and RON, the VREF
levels, per-group drive strengths and slew compensation for DATA, CMD, CLK and
CTL, the ODT latencies and delays, and the four DFE taps.

**Misc** (81 rows) — the per-channel read latencies, the power-down and
command configuration, ECS state, the controller feature switches, the
preamble and postamble settings, the refresh arbitration controls, and the
DDR5 mode registers. Drawn as one column: these are settings to read down
rather than a set to compare across.

**Telemetry window** (47 rows) — everything that moves, kept out of the tabs
so the tabs stay still. Clocks including one row per logical processor,
temperatures, package and core power, board voltages, the graphics card, each
DIMM's PMIC rails, and the count of hardware errors Windows has logged. Every
reading carries its minimum, maximum and average over the run, with a timer
and a reset.

**Advanced window** — every row from the four reading tabs in one searchable
list, filtered as you type, for when you know the name of the field you want.
Its **Dump** button writes the lot to `RochViewer.txt` on the desktop, one
`name : value` per line, so a configuration can be kept or posted without
screenshotting five tabs. A row that could not be read is written as `N/A`
rather than dropped: a dump that quietly omits what it missed is one you
cannot trust to be complete.

## How it reads

Each value comes from the part that owns it:

- **Memory timings** — Intel MCHBAR. On DDR5 the second module is a
  sub-channel of the same controller, so its registers sit `0x800` from the
  first rather than in a second controller.
- **Module identity** — the SPD5 hub on each DIMM, over the PCH SMBus: part
  number, serial, manufacture date, DRAM maker and die. The identity block is
  on a page that has to be selected first, and a board with SPD Write Disable
  armed refuses the ordinary write, so the page select is issued as a Process
  Call, which carries its write phase past an interlock that gates on the
  transaction's direction bit.
- **DIMM and board sensors** — the SPD hub's thermal sensor and each module's
  PMIC over the same bus, and the board's Super I/O over the LPC bus.
- **Package power** — the RAPL energy counters, read from MCHBAR rather than
  from the MSRs they are usually reached through, because the driver this
  project uses offers no RDMSR.
- **CPU and platform identity** — CPUID through WMI, SMBIOS, the firmware's
  own tables, and PCI configuration space for the host bridge and PCH.
- **Graphics** — the adapter's PCI identity, with NVAPI and NVML for the frame
  buffer, its type and maker, the bus width, Resizable BAR, the driver, and
  the card's live clocks, temperature, fan, power and voltage.

## What it will not do

The reading paths are deliberately narrow, and the rules are enforced in code
rather than by convention:

- **Reads, not writes.** The SMBus transports assemble the address byte with
  the read direction bit set from a path that has no direction parameter. The
  only writes anywhere are selectors that choose what the next read returns —
  the PMIC's ADC channel and the SPD hub's page — each restored afterwards,
  each fixed to one register that no caller can redirect. Nothing that
  configures a rail or writes an SPD array is reachable.
- **No guessed values.** A decode table names a code that was read; where a
  code is unrecognised the raw value is shown instead. Where a figure cannot
  be read at all it is left blank rather than filled from a plausible
  neighbour, and the two rows sourced from a table rather than a reading are
  marked as such and report nothing for hardware they do not list.
- **No stolen buses.** Access is serialised on the same named mutexes the
  mainstream monitoring tools take for SMBus and for PCI configuration space,
  and multi-step reads hold the bus for the whole sequence so another tool
  cannot change the state they depend on midway.
- **Fields are located, not assumed.** Registers this project added were found
  by diffing full captures across a controlled change — a BIOS setting, a load
  cycle — or by matching every field of a register at once against a reference
  tool, and the evidence is recorded beside the definition.

## Build and run

1. Install 64-bit Python 3.13 with Tcl/Tk.
2. `py -V:3.13 -m pip install -r requirements.txt`
3. Put `inpoutx64.dll` and `inpoutx64.sys` in the project directory.
4. Run from source with `pyw -V:3.13 run_viewer.py`, or build with
   `py -V:3.13 -m PyInstaller --clean -y RochViewer.spec` and run
   `dist\RochViewer.exe`.
5. Accept the administrator prompt.

`pyw` is the windowed launcher, so the viewer comes up on its own without a
console behind it — the same as the built EXE, which is already windowed. Use
`py` instead when you want that console: the diagnostics the tool prints when a
register or a transport does not answer go to stdout, and under `pyw` there is
nowhere for them to land.

Run the tests with `py -V:3.13 -m unittest discover -s tests -t .`. A handful
skip where they need hardware or a display that the running machine does not
have.

## Licence

GPL-3.0-or-later. See `LICENSE`.

This program is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
FOR A PARTICULAR PURPOSE. It reads hardware registers directly and requires a
kernel-level access driver to do so; run it on hardware you are willing to
experiment with.
