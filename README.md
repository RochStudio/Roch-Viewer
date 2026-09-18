# Roch Viewer 1.0.4

Version 1.0.4 is the current development version. The published 1.0.3 release remains available from [Releases](https://github.com/RochStudio/Roch-Viewer/releases/tag/v1.0.3).

1.0.4 removes duplicate DDR4 DQ/DQS ODT rows, shows one DRAM RON row, and adds raw MR1 evidence to Advanced/Dump. On the tested ASUS Z790-A D4 BIOS 3202 / 0x11F system, the user confirmed matching 34/48-ohm readouts with **DIMM RON Training** and **MRC Fast Boot** disabled. See [DDR4 RON validation](docs/ddr4-ron-validation.md) for the evidence and scope.

A read-only Windows memory-timing and hardware-monitoring tool for supported Intel and AMD systems. View clocks, timings, per-channel settings, RAM details and native CPU/memory telemetry in a compact light/dark interface. No HWiNFO dependency.

## Install

1. Download `RochViewer.exe` from [Releases](https://github.com/RochStudio/Roch-Viewer/releases).
2. Obtain `inpoutx64.dll` from [Highresolution Enterprises](https://www.highrez.co.uk/downloads/inpout32/) and place it beside the executable. It is **not bundled**.
3. Run `RochViewer.exe` as administrator.

Windows 10/11 x64 is required. The packaged EXE needs no Python or .NET installation.

## Prerequisites

The InpOut DLL contains a kernel driver and can install a persistent Windows service on first elevated use. Windows security policies may block it; hardware-register readings then remain unavailable. Do not disable protections just to load it. Read the [driver details](docs/reference.md#prerequisites).

## Features

- **Summary:** CPU/board identity, memory speed, DRAM Ratio below BCLK, supported MCLK/UCLK/FCLK readings, key timings and voltage snapshots. Select one module or all modules; matching channel values display once and differences display side by side.
- **System Info:** system, processor, motherboard, clocks and RAM details, followed by graphics-card identity at the bottom.
- **Timings:** primary, secondary and tertiary timings, including per-channel values where exposed, with aligned alternating row shading and scrolling at the compact window size.
- **Voltages:** startup snapshots of supported CPU, motherboard and memory rails. Use Telemetry for live readings.
- **Training and IMC:** supported RTT/ODT, drive strengths, VREF, training and integrated-memory-controller configuration fields. AMD IMC includes native preamble/postamble and ECC status (memory-controller ECC, not DDR5 on-die ECC). Granite Ridge also exposes eight raw training codes with compatibility labels; those labels are not verified physical VREF/DFE readings. Unavailable reads stay blank.
- **Native telemetry:** CPU/effective clocks, temperatures, power and supported board voltages, plus each DIMM's temperature and PMIC rails. Current/minimum/maximum/average statistics and reset controls are included.
- **Per-stick RAM details:** part number, capacity, rank and available IC information in the footer and telemetry.
- **Shared Roch interface:** light/dark themes, consistent toolbar buttons, and YouTube | X | Discord links at the bottom-left.
- **Advanced view:** searchable fields and a text dump for comparing configurations or reporting issues.
- **Read-only:** no overclocking controls or voltage writes. Low-level selectors and query transactions retrieve readings only.

Version 1.0.3 adds reorganized timing and training layouts, independent A1/B1 module readings, IMC and RTL views, expanded System Info, snapshot voltage reporting, and a compact 750 × 775 interface with scrolling for longer pages.

## Screenshots

<img src="assets/screenshots/amd-summary.png" alt="AMD Summary with per-stick RAM details" width="700">

<img src="assets/screenshots/amd-telemetry.png" alt="Native AMD clocks, temperatures, power and voltages" width="550">

<img src="assets/screenshots/amd-memory-telemetry.png" alt="AMD VDDIO and per-DIMM identity, temperatures and PMIC rails" width="550">

Actual updated local-build screenshots, not UI concepts. Values shown are examples, not tuning recommendations.

## Hardware support

Intel LGA1700/LGA1851 and supported AMD AM5/Granite Ridge systems. Readings depend on CPU, memory type, motherboard, BIOS and driver access. Unsupported values stay unavailable; board sensor mappings are not assumed to transfer between models. See [validation history and limitations](docs/reference.md).

## Build from source

Install **Python 3.13 x64** with Tkinter, then run:

```powershell
py -3.13 -m pip install -r requirements.txt
py -3.13 -m PyInstaller -y RochViewer.spec
```

Output: `dist\RochViewer.exe`. Put the separately obtained `inpoutx64.dll` beside it. To run from source, put the DLL beside `run_viewer.py` and use `pyw -3.13 run_viewer.py`.

Tests: `py -3.13 -m unittest discover -s tests -t .`

## Credits

- **Ivan Rusanov (irusanov) — [ZenStates-Core](https://github.com/irusanov/ZenStates-Core) and [ZenTimings](https://github.com/irusanov/ZenTimings):** AMD register, timing and SMU references.
- **Highresolution Enterprises / Logix4u — InpOut32/64:** separately supplied low-level access component.
- **[LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor):** board-sensor register and mapping references.
- **Zorko — gnr-smu:** independent Granite Ridge SMU mailbox research.

Created by **Roch Studio / MateoPCTech**.

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)

[License](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.txt) · [Detailed reference](docs/reference.md)

> Low-level hardware access carries risk, even in a read-only tool. Provided as-is, without warranty.
