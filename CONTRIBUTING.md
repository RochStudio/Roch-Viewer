# Contributing

## The thing to know first

A passing test suite here does **not** mean a change is correct.

Most of what this project does is read hardware registers, and a continuous
integration runner has no memory controller, no Super I/O and no driver. The
tests that need those skip themselves, so CI checks the decoding, the table
construction and the layout — real work, and not the part that is easy to get
wrong.

The part that is easy to get wrong is claiming a register means something. That
can only be checked on hardware, against a second instrument, and the bar this
project holds itself to is in the README under "What it will not do":

> Registers this project added were found by diffing full captures across a
> controlled change — a BIOS setting, a load cycle — or by matching every field
> of a register at once against a reference tool, and the evidence is recorded
> beside the definition.

If you add or change a register, please record how you established it, in a
comment next to it. "It reads a plausible number" is not how, and a value that
happens to look right on one machine is the failure this rule exists to
prevent.

## Running the tests

```
py -m pip install -r requirements.txt
py -m unittest discover -s tests -t .
```

Three or so will skip without hardware or a display. On an Intel DDR5 bench
with the driver present, everything runs.

## Running the viewer

You need `inpoutx64.dll` — see "Prerequisites" in the
README. They are not distributed with this project.

## Layout

Modules are grouped by what they talk to, under `rochviewer/`:

| folder | what is in it |
| --- | --- |
| `hardware` | the driver, port and physical-memory access, PCIe config space |
| `intel` | MCHBAR timings, the PCH SMBus, board sensors, RAPL power |
| `memory` | SPD identity, the DIMM thermal sensors and PMICs, the inventory |
| `sensors` | Super I/O chips, the EC, CPU clocks, rails, Windows error log |
| `gpu` | the graphics card |
| `ui` | the window, the searchable list, the telemetry pop-out |

The dispatcher and the platform profile sit at the package root, because they
decide which of the rest is allowed to run at all. `run_viewer.py` at the
repository root is the entry point, and the one PyInstaller is pointed at.

## Scope

This repository covers Intel LGA1700 and LGA1851, and AMD AM5 / Granite Ridge.

The AMD backend follows ZenStates-Core and ZenTimings, which is permitted
because this project is GPL-3.0 as they are, and it comes with obligations
that are not optional: if you change a file carrying the derived-work notice,
leave the notice alone, and if you add a register that came from that work,
say so beside it. Attribution that is quietly dropped is the one contribution
that will not be merged.

What is deliberately absent are the write-capable SMU diagnostics: they issue
fixed command tuples to the AMD SMU mailbox, were gated to one exact CPU, and
are bench instruments rather than anything a viewer needs. Patches adding a
generic mailbox sender will not be merged.

Reports of a register being wrong are more valuable than most features. If a
reading here disagrees with CPU-Z, HWiNFO, VoidTimings or the ASRock Timing
Configurator on your machine, that is worth an issue — say which tool, which
row, both values, and your board and CPU.
