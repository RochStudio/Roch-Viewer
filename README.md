# Roch Viewer 1.0.1

Roch Viewer is a Windows memory-controller and timing viewer for Intel and AMD
desktop platforms. It reads the memory controller, the modules, the board and
the CPU directly, and shows what they actually report — the timings the
controller was programmed with, the training results it settled on, and the
sensors that move while the machine runs.

It is a read-only viewer. It changes no setting, and nothing it shows is a
value it was told to expect.

## Download

A built `RochViewer.exe` is attached to every release:

**https://github.com/RochStudio/Roch-Viewer/releases**

It needs one file that is deliberately not in the download — see
[Prerequisites](#prerequisites). Building it yourself is a few sections below.

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

It has also been exercised on a Gigabyte Z890 AORUS TACHYON ICE with a Core
Ultra 7 270K Plus, an MSI Z790MPOWER with an i5-14600KF, and an LGA1700 DDR4
board. Each found faults the primary bench could not, which is the argument
for reading a second board rather than a fact about these ones: every memory
clock read half its value in Gear 4, four timings were unreachable across a
32-bit register boundary, three command-bus terminations printed as reserved
against a gap in a decode table, and the Super I/O rail map derived on one
board named a different rail on the next — which is why that map is chosen by
board model now. The channel count needed two boards before it was right, a
Z790 and a Z890 reporting it differently and each correct for its own
platform.

None of these is a daily target, so treat them as checked rather than
continuously validated. The DDR4 mode-register shadow in particular is
inferred rather than documented and rests on cross-checks from a single board.

The AMD side was developed and validated on an MSI B850MPOWER with a Ryzen
9850X3D, against same-boot ZenTimings. The code has been exercised on that
board since it came into this repository, and bugs found there have been
fixed — but **no reading has been compared against a reference tool since the
move**, so confirm anything you intend to rely on against a second tool on
your own board. Its electrical values — RTT, ODT and the drive strengths —
come from a reverse-engineered APOB parser whose channel attribution is
verified only for the two-record geometry observed on that bench.

The AMD backend follows ZenStates-Core and ZenTimings by irusanov, both
GPL-3.0, which is why this project is GPL-3.0 too. See
`THIRD_PARTY_NOTICES.txt`.

## Prerequisites

- Windows 10 or 11, 64-bit.
- Administrator rights. Low-level register access requires them.
- **`inpoutx64.dll`**, which is not distributed here. Get it from
  Highresolution Enterprises (www.highrez.co.uk) and put it beside
  `RochViewer.exe`, or beside `run_viewer.py` to run from source. See
  `THIRD_PARTY_NOTICES.txt` for why it is not bundled.

  That one file is all you need — the "Binaries only" archive on the
  download page contains no `.sys`, and does not need to.

If the driver is missing the program still opens — the CPU, the board, the
BIOS and the module identity all come from Windows and need no driver — but
every register-backed row reads nothing. It says so in a strip across the top
and names the directories it looked in, so a copy in the wrong place is
visible rather than silent.

**Installing it is not just a file copy.** The DLL carries the kernel driver
inside itself as a resource, which is why it is the only file you need. The
first time it runs elevated it writes that driver to
`System32\Drivers\inpoutx64.sys` and registers it as an automatic-start Windows
service, which stays there afterwards whether or not this program is ever run
again. Removing it means stopping and deleting the `inpoutx64` service and
deleting that file, not deleting the folder you downloaded.

That component is also **no longer maintained**: its author says so on the
download page and that he can no longer sign the driver. It works, it is
signed, and every comparable tool depends on it or something like it — but it
is not something anyone is fixing.

Windows security software may warn about that driver. It is a well-known
low-level access component and is also, for the same reason, a component
attackers have abused; decide for yourself whether you want it on your
machine. The released EXE is unsigned, so SmartScreen will prompt on first
run.

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

**Misc** (83 rows) — the per-channel read latencies, the power-down and
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
  writes that do exist are the ones a read cannot happen without, and each is
  pinned to a fixed destination no caller can redirect:
  - **Selectors**, which choose what the next read returns — the PMIC's ADC
    channel and the SPD hub's page — each restored afterwards.
  - **The AMD SMU mailbox.** On AMD, telemetry is not memory-mapped: the SMU
    is asked for it, and asking means writing a message ID and its arguments
    to the SMN data window. Four command IDs are permitted, listed in one
    tuple that the sender checks against — table version, table address, table
    transfer, and GetPBOScalar. Every one is a query. No message that sets a
    limit, a voltage or a frequency is reachable, and the write-capable SMU
    diagnostics this project was developed with are deliberately absent from
    this repository.

  Nothing that configures a rail or writes an SPD array is reachable.
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

## Getting it running

There are two ways in. Both need the driver, and both need Administrator.

### The quick way: the released EXE

1. **Download `RochViewer.exe`** from the
   [releases page](https://github.com/RochStudio/Roch-Viewer/releases).
   Nothing to install; it is a single file.
2. **Get the driver.** Download the InpOut32/64 binaries from
   [highrez.co.uk](http://www.highrez.co.uk/downloads/inpout32/) — the
   "Binaries only" archive is enough — and take **`inpoutx64.dll`** out of it.
   That single file is all this needs; the kernel driver is inside it.
3. **Put it in the same folder as `RochViewer.exe`.** Not a subfolder, not
   somewhere on PATH: beside it.
4. **Right-click `RochViewer.exe` → Run as administrator.** Windows will
   prompt; the tool cannot read a register without it.

First run will also raise SmartScreen, because the EXE is unsigned — *More
info* → *Run anyway*, or don't, if you would rather build it yourself. Your
antivirus may object to the driver as well; see
[Prerequisites](#prerequisites) for why.

If the top of the window shows a red strip saying `inpoutx64.dll not found`,
step 2 or 3 did not take. The strip names the folders it looked in.

### From source

1. **Install Python 3.13, 64-bit**, from
   [python.org](https://www.python.org/downloads/windows/). Take the
   *Windows installer (64-bit)*, and during setup leave **tcl/tk and IDLE**
   ticked — the interface is Tkinter and will not start without it. Ticking
   *Add python.exe to PATH* is convenient but not required; the commands below
   use the `py` launcher, which the installer always provides.

2. **Get the code.**

   ```
   git clone https://github.com/RochStudio/Roch-Viewer.git
   cd Roch-Viewer
   ```

   Or download the ZIP from the repository page and extract it.

3. **Install the four dependencies.**

   ```
   py -V:3.13 -m pip install -r requirements.txt
   ```

   That is `customtkinter` for the interface, `wmi` and `pywin32` for the
   Windows queries, and `pyinstaller` only if you want to build an EXE.

4. **Put `inpoutx64.dll` in the project folder**, beside `run_viewer.py`.
   Same file, same source as step 2 of the quick way, and the same caveat
   about the service it installs.

5. **Run it.**

   ```
   pyw -V:3.13 run_viewer.py
   ```

   Accept the administrator prompt. If you started from an ordinary,
   non-elevated prompt, that prompt is where the elevation happens.

To build your own EXE instead:

```
py -V:3.13 -m PyInstaller --clean -y RochViewer.spec
```

It lands in `dist\RochViewer.exe`. The driver has to sit beside *that* copy
too — building does not move it.

### Which launcher, and why

`pyw` is the windowed launcher, so the viewer comes up without a console
behind it — the same as the built EXE, which is already windowed. Starting
with `py` from an ordinary prompt gets you there too: accepting the
administrator prompt starts a fresh process, and that one is windowed
whichever launcher you used.

Use `py` from an **already elevated** prompt when you want the console. There
is no elevation step to pass through, so the process keeps the one it was
given, and the diagnostics the tool prints when a register or a transport does
not answer have somewhere to land. Under a windowed launcher they go nowhere.

### Running the tests

```
py -V:3.13 -m unittest discover -s tests -t .
```

A handful skip where they need hardware, a platform or a display the running
machine does not have — a test gated to AM5 skips on an Intel bench, and the
reverse. GitHub Actions runs the whole suite on Windows for every push.

## Licence

GPL-3.0-or-later. See `LICENSE`.

This program is distributed in the hope that it will be useful, but WITHOUT
ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
FOR A PARTICULAR PURPOSE. It reads hardware registers directly and requires a
kernel-level access driver to do so; run it on hardware you are willing to
experiment with.
