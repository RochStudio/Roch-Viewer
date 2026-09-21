# Roch Viewer 1.0.5

Version 1.0.5 is the current published release and is available from [Releases](https://github.com/RochStudio/Roch-Viewer/releases/tag/v1.0.5).

Roch Viewer is a read-only Windows memory-timing and hardware-monitoring tool for supported Intel and AMD systems. It displays clocks, timings, per-channel settings, RAM identity, motherboard sensors and native CPU/memory telemetry in a compact light/dark interface. It does not require HWiNFO.

Version 1.0.5 expands DDR4 support and reorganizes the interface:

- Shows the raw DDR4 controller `tRFCpb` field to match the reference viewer;
  it normally reads `0` because DDR4 uses all-bank refresh. `VDD2` remains
  hidden on DDR4.
- Shows DDR4 RTT and RON in resistance-first RZQ notation, such as `80 RZQ/3`.
- Reads separate A2/B2 module identity on supported four-DIMM ASUS boards and reports module vendor, DRAM manufacturer/die, part number, serial number and manufacture date.
- Decodes supported DDR4 MR0-MR6 training, power and operating fields without exposing raw RON shadow diagnostics in Advanced.
- Reports the ASUS Z790-A D4 DRAM rail and the complete supported Nuvoton NCT6798D voltage and temperature set in Telemetry.
- Uses fixed-width, unscrolled main tabs with per-tab heights and keeps the window's top edge fixed while switching tabs.
- Opens Advanced beside the right edge of the viewer and Telemetry beside the left edge when screen space permits.

DDR4 RON is decoded from controller MR1 shadows. On the tested ASUS Z790-A D4 BIOS 3202 / microcode 0x11F system, 34/48-ohm BIOS selections produced matching displayed values with **DIMM RON Training** and **MRC Fast Boot** disabled. See [DDR4 RON validation](docs/ddr4-ron-validation.md) for the evidence and scope.

## Install

1. Download `RochViewer.exe` from [Releases](https://github.com/RochStudio/Roch-Viewer/releases).
2. Obtain `inpoutx64.dll` from [Highresolution Enterprises](https://www.highrez.co.uk/downloads/inpout32/) and place it beside the executable. It is **not bundled**.
3. Run `RochViewer.exe` as administrator.

Windows 10/11 x64 is required. The packaged EXE needs no Python or .NET installation.

## Prerequisites

The InpOut DLL contains a kernel driver and can install a persistent Windows service on first elevated use. Windows security policies may block it; hardware-register readings then remain unavailable. Do not disable protections just to load it. Read the [driver details](docs/reference.md#prerequisites).

## Features

- **Summary:** CPU name, cores/threads, microcode, motherboard model/BIOS, memory speed, supported clock ratios, key timings, RTT/RON/VREF and voltage snapshots. Select one module or all modules; matching channel values display once and differences display side by side.
- **System Info:** system, processor and motherboard identity on the left, with configured clocks and graphics identity on the right.
- **SPD:** select a physical DIMM slot to see system channels and capacity, module identity and geometry, maximum JEDEC bandwidth, DDR4 JEDEC timing choices, and Intel XMP 2.0 profiles read directly from the module.
- **Timings:** primary, secondary and tertiary timings, including per-channel values where exposed, balanced across three columns without scrolling.
- **Voltages:** startup snapshots of supported CPU, motherboard and memory rails. Use Telemetry for live readings.
- **Training and IMC:** supported RTT/ODT, RON, ODT delay, VREF, drive strength, power-down, refresh, mode-register and integrated-memory-controller configuration fields. Independent module/channel values remain visible where the hardware exposes them. AMD IMC includes native preamble/postamble and ECC status (memory-controller ECC, not DDR5 on-die ECC). Granite Ridge also exposes eight raw training codes with compatibility labels; those labels are not verified physical VREF/DFE readings. Unavailable reads stay blank.
- **RTL:** compact per-memory-controller/channel round-trip-latency values.
- **Native telemetry:** CPU/effective clocks, temperatures, power and supported motherboard voltages, plus each DIMM's temperature and PMIC rails. On the ASUS Z790-A D4, supported NCT6798D readings include Vcore, +5V, AVSB, 3VCC, +12V, VIN inputs, standby/battery rails, VTT, DRAM, CPU L2, CPU VCCSA, CPU AUX and motherboard/CPU/PCH temperatures. Current/minimum/maximum/average statistics and reset controls are included.
- **Per-stick RAM details:** part number, capacity, rank and available IC information in the footer and telemetry.
- **Shared Roch interface:** light/dark themes, a sun/moon title-bar control, consistent toolbar buttons and YouTube | X | Discord links at the bottom-left.
- **Advanced view:** searchable decoded fields and a text dump for comparing configurations or reporting issues.
- **Read-only:** no overclocking controls or voltage writes. Low-level selectors and query transactions retrieve readings only.

## Window layout

Every main tab is 750 pixels wide. Heights are sized to the content and grow from the bottom, so switching tabs does not move the title bar:

| Tab | Size |
| --- | ---: |
| Summary | 750 × 750 |
| System Info | 750 × 750 |
| SPD | 750 × 750 |
| Timings | 750 × 775 |
| Training | 750 × 800 |
| IMC | 750 × 1100 |
| RTL | 750 × 654 |
| Voltages | 750 × 654 |

On shorter displays, the app caps the requested height to the available desktop area.

## Screenshots

<img src="assets/screenshots/amd-summary.png" alt="AMD Summary with per-stick RAM details" width="700">

<img src="assets/screenshots/amd-telemetry.png" alt="Native AMD clocks, temperatures, power and voltages" width="550">

<img src="assets/screenshots/amd-memory-telemetry.png" alt="AMD VDDIO and per-DIMM identity, temperatures and PMIC rails" width="550">

Actual updated local-build screenshots, not UI concepts. Values shown are examples, not tuning recommendations.

## Hardware support

Intel LGA1700/LGA1851 and supported AMD AM5/Granite Ridge systems. Readings depend on CPU, memory type, motherboard, BIOS and driver access. DDR4 and DDR5 expose different fields; unsupported values stay unavailable, and motherboard sensor mappings are not assumed to transfer between models. See [validation history and limitations](docs/reference.md).

## Build from source

Install **Python 3.12 or 3.13 (x64)** with Tkinter, then run:

```powershell
py -3.12 -m pip install -r requirements.txt
py -3.12 -m PyInstaller -y RochViewer.spec
```

Output: `dist\RochViewer.exe`. Put the separately obtained `inpoutx64.dll` beside it. To run from source, put the DLL beside `run_viewer.py` and use `pyw -3.12 run_viewer.py`.

Tests: `py -3.12 -m unittest discover -s tests -t .`

## Credits

- **Ivan Rusanov (irusanov) — [ZenStates-Core](https://github.com/irusanov/ZenStates-Core) and [ZenTimings](https://github.com/irusanov/ZenTimings):** AMD register, timing and SMU references.
- **Highresolution Enterprises / Logix4u — InpOut32/64:** separately supplied low-level access component.
- **[LibreHardwareMonitor](https://github.com/LibreHardwareMonitor/LibreHardwareMonitor):** board-sensor register and mapping references.
- **Zorko — gnr-smu:** independent Granite Ridge SMU mailbox research.

Created by **Roch Studio / MateoPCTech**.

[YouTube](https://www.youtube.com/@MateoPcTech) | [X](https://x.com/MateoPCTech) | [Discord](https://discord.gg/KfzExpKQHB)

[License](LICENSE) · [Third-party notices](THIRD_PARTY_NOTICES.txt) · [Detailed reference](docs/reference.md)

> Low-level hardware access carries risk, even in a read-only tool. Provided as-is, without warranty.
