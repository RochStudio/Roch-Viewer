# Roch Viewer on Linux

The same window and the same readings as on Windows. Wayland and X11 both work.

> It reads hardware registers directly, and needs root-level access to do that. It writes nothing but the selectors a read cannot happen without, listed under [What it can touch](#what-it-can-touch).

Verified on a Gigabyte Z890 AORUS TACHYON ICE with a Core Ultra 7 270K Plus, DDR5-6000 and two RX 7900 XTs, on kernel 7.2 and Hyprland 0.56, with its readings checked against the kernel's own and against what the Windows build records for that board. **Nothing else has been run on real hardware yet.** LGA1700, AM5 and NVIDIA share the decoders and pass their tests, but their paths through the kernel have not been tried on those machines.

## Install

**From a release.** Download `RochViewer-<version>-linux-x86_64.tar.gz` from [Releases](https://github.com/RochStudio/Roch-Viewer/releases/latest), unpack it, and in the folder it unpacks to run:

```bash
./rochviewer install
```

It needs glibc 2.35 or newer, which means Ubuntu 22.04, Debian 12, Fedora 36 or anything since. `SHA256SUMS.txt` on the release page holds the package's checksum.

**From source.** You need [Rust stable](https://rustup.rs). No other development packages are required.

```bash
cargo xtask install
```

A binary built this way needs the glibc of the machine that built it, so on a very current system it may not start on an older one.

Either way it installs for your user, with no root needed:

| File | What |
| --- | --- |
| `~/.local/bin/rochviewer` | The program |
| `~/.local/share/applications/RochViewer.desktop` | The launcher entry |
| `~/.local/share/icons/hicolor/256x256/apps/RochViewer.png` | The icon |

Use `--prefix PATH` to install somewhere else, and `rochviewer uninstall` to remove them.

## Access

The window runs as you. A small helper does the register reads as root, and the two talk over a pipe. The helper is this same program, started through `pkexec`, so opening the viewer brings up your desktop's password prompt, the same question Windows asks with UAC.

If you decline, the window still opens and says why. The CPU, the board, the BIOS and the graphics card still read; everything from the memory controller is blank.

**To stop being asked,** run this once:

```bash
sudo ~/.local/bin/rochviewer setup
```

(`sudo rochviewer` usually fails on a home install, because sudo only searches its own short path.)

It copies the program to `/usr/local/libexec/rochviewer-helper`, owned by root and executable only by your group, and gives that copy the capabilities it needs. Under that name the program is only ever the helper. It also writes `/etc/modules-load.d/rochviewer.conf`, so `/dev/i2c-*` exists after a reboot.

That is a real trade. Anything running as you can then start the helper without a password, and do what it allows: read device registers and firmware tables, reach I/O ports above 0x200 and make the narrow SMBus and PCI writes listed [below](#what-it-can-touch). It cannot read or write RAM or files, but those ports include the SMBus controller and the Super I/O. Leave it at the prompt if that matters on your machine.

```bash
rochviewer setup --status
```

```bash
rochviewer setup --show
```

```bash
sudo ~/.local/bin/rochviewer setup --undo
```

Run the setup again after updating the viewer. A helper left over from an older version is detected and passed over, and the viewer goes back to asking.

### If it says it cannot get access

**Kernel lockdown.** Secure Boot usually turns this on, and it blocks `/dev/mem` and the I/O ports for every process, root included. `cat /sys/kernel/security/lockdown` shows it. Register readings are not possible while it is on.

**`Failed to map physical address`.** The kernel refuses `/dev/mem` for a region a driver has claimed. This has not been seen for the memory controller's registers, because nothing claims them. If it happens, `iomem=relaxed` on the kernel command line lifts it.

**No prompt, and a notice saying there is no polkit agent.** A desktop without an authentication agent has nothing to show the prompt with. `sudo rochviewer setup` avoids needing one.

## Command line

```text
rochviewer                 open the window
rochviewer dump [FILE]     every row as JSON, to FILE or the terminal
rochviewer install         add it to your launcher, for this user (--prefix PATH to choose where)
rochviewer uninstall       take it out again
rochviewer setup           (as root) stop asking for access at each launch
rochviewer setup --status  whether that is set up
rochviewer setup --undo    (as root) remove it
```

`dump` is the thing to attach to a bug report. It works without a desktop.

## What differs from Windows

| | Windows | Linux |
| --- | --- | --- |
| Asking for permission | UAC at each launch, whole program elevated | `pkexec` at each launch or `rochviewer setup` once, only the helper elevated |
| Driver | `inpoutx64.dll`, supplied by you | None to install: `/dev/mem`, the I/O ports, sysfs and `/dev/i2c-*` |
| Identity | WMI and the registry | `/proc`, `/sys`, SMBIOS, CPUID |
| Hardware errors row | WHEA events in the event log | Machine checks, PCIe AER and EDAC in the kernel log. Blank if you cannot read the journal |
| CPU clocks | PDH counters | `cpuinfo_avg_freq` or `scaling_cur_freq`, and `/proc/stat` |
| GPU details | NVAPI, NVML, ADL, ADLX, the registry | NVML; for Radeon, `amdgpu` through sysfs and its query ioctl |
| Radeon live rows on an Intel board | Blank | Read |
| Radeon memory clock | As ADL reports it | As `amdgpu` reports it: for GDDR6, half of what Windows tools show |
| NVIDIA code name and core voltage | NVAPI | Blank: NVAPI is not there |
| Font | Consolas | Consolas if installed, otherwise your system's monospace, spaced onto the same grid |
| Window position, opacity | Remembered, adjustable | Up to your compositor |
| Drop-down menus | Their own window, past the window's foot | Inside the window: Wayland lets a client place nothing |
| Tiling compositors | Not applicable | Windows open floating. On Hyprland a tab change resizes through its control socket |
| Screenshot to clipboard | Built in | Needs `wl-clipboard` on Wayland, `xclip` on X11 |
| Dump and screenshots | Desktop, Pictures | The folders in `user-dirs.dirs`; the dump goes to your home folder if there is no Desktop |
| Settings | `%LOCALAPPDATA%\RochViewer` | `~/.config/RochViewer` |

## What it can touch

The Windows build's rule, that there is no general write API, is enforced in the helper, so it holds however it is asked:

- **`/dev/mem` is opened read-only**, so a physical write has nowhere to go. **RAM cannot be read:** the helper refuses the ranges the kernel calls System RAM, whatever the kernel's own settings would allow.
- **PCI configuration space goes through sysfs**, not the CF8/CFC ports, which the kernel uses under a lock nothing else can take. A write can reach two registers on one device: the AMD SMN selector and data window.
- **The legacy ports below 0x200 are refused**, except the Super I/O and EC index pairs, and 0xCF8-0xCFF always are. 0xCF9 restarts the machine.
- **SMBus transfers go through the same allowlists** as on Windows: reads from the thermal sensors, PMICs and SPD hubs, and writes only to a PMIC's ADC selector and a hub's page selector.
- **A card powered down to D3cold is left alone.** Reading it would wake it, and on some machines a card that fails to come back hangs everything that touches it.

### Sharing the hardware with the kernel

On Linux the kernel is using much of this hardware itself, and there is no lock to share with it from outside.

**SMBus.** Every transfer goes through `/dev/i2c-*` where the kernel has an adapter, which `i2c_i801` and `i2c_piix4` provide, so its lock serialises them. There is one exception. With SPD Write Disable armed, which it is on most boards, the kernel's SPD driver can only ever see page 0 of a module's SPD. The window gets past that the way the Windows build does, with one raw Process Call per page. That transaction goes directly to the controller without the kernel's lock. It is rare, happening only when an SPD is read, and each takes about a millisecond. Its effect on the kernel is nil: the hub's temperature register, which is what the kernel reads, does not depend on the page.

Without `i2c-dev` loaded there is no `/dev/i2c-*`. The helper loads it if it is running as root; with `rochviewer setup` the setup arranges it. If it still is not there, the raw transport is used for everything.

**Super I/O.** If the `it87` or `nct6775` driver is bound, the viewer does not touch the chip, and the board's voltage, temperature and fan rows stay blank. Interleaving with those drivers can read the wrong bank and can wedge the embedded controller on the chips that have one. Unload the driver to get those rows back.

**SMN (AMD).** The index and data registers on the host bridge are one pair, and the kernel's `k10temp` and others use it under a lock that cannot be shared from here. The reads and the SMU's query messages go through it regardless, as they do beside other tools on Windows, and this path has not been run on AM5 hardware under Linux. Treat readings there as unverified.

**ACPI EC.** The VRM temperature on ASUS boards is read through the kernel's `ec_sys` module, which goes through its EC driver, rather than from the ports that driver owns. The helper loads the module if it is running as root.
