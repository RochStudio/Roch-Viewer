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

You need `inpoutx64.dll` and `inpoutx64.sys` — see "Prerequisites" in the
README. They are not distributed with this project.

## Scope

This repository covers Intel LGA1700 and LGA1851. It does not cover AMD, and
patches adding an AMD backend will not be merged here: the earlier
implementation had a provenance problem that could not be resolved, and
reintroducing one is not something this repository can accept on trust.

Reports of a register being wrong are more valuable than most features. If a
reading here disagrees with CPU-Z, HWiNFO, VoidTimings or the ASRock Timing
Configurator on your machine, that is worth an issue — say which tool, which
row, both values, and your board and CPU.
