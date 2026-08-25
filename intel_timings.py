import wmi
import winreg
import os
import sys
import re
from functools import lru_cache, partial, wraps
from pathlib import Path
from read import read_timing, read_physical_memory_int
from display_values import is_dual_timing
# Classification only: this module performs no privileged access and is the
# same one that selected this backend, so the Platform row cannot disagree
# with the code that is running.
from platform_profiles import (
    LGA1700_DDR4,
    LGA1700_DDR5,
    LGA1851,
    detect_current_platform,
)

MCHBAR = 0xFEDC0000
MCHBAR2 = 0xFEDD0000

# Platforms whose modules refresh the DDR5 way.
DDR5_TIMING_PLATFORMS = (LGA1700_DDR5, LGA1851)


@lru_cache(maxsize=None)
def active_platform():
    """The platform id, reusing whatever timings already resolved.

    detect_current_platform opens a WMI connection and runs three queries --
    about a second on the bench. timings assigns ACTIVE_PLATFORM before it
    imports this backend, so inside the viewer the answer already exists by
    the time this module is loaded and asking again would add that second to
    every start. Read from sys.modules rather than imported: importing timings
    from the backend it is in the middle of importing would be circular.
    """
    import sys

    profile = getattr(sys.modules.get("timings"), "ACTIVE_PLATFORM", None)
    return profile or detect_current_platform()


# The two columns the tabs draw are the two installed modules, and how far
# apart their registers sit depends on the memory generation.
#
# DDR5 puts two channels inside one controller, 0x800 apart, and a module
# drives one of them. The four selections the reference tools offer resolve
# there, checked against their own DFE tap readings on the DDR5 bench:
#
#   MCHBAR          MC0 CHA A1   -18 -3 +0 -3
#   MCHBAR2         MC1 CHA A1   -22 -2 -1 +0
#   MCHBAR + 0x800  MC0 CHB B1   -21 +2 -4 +0
#   MCHBAR2 + 0x800 MC1 CHB B1   -16 +0 -4 -2
#
# DDR4 has no sub-channels. Its second module is on the second controller, one
# 0x10000 window up, and the 0x800 block is a channel that was never trained.
# Measured on the Z790-P with a module in each channel, the RTL rows -- a
# per-DIMM trained latency, reading 25 where a slot is empty -- report:
#
#   MC0 CHA  77     MC1 CHA  79      the two installed modules
#   MC0 CHB  25     MC1 CHB  25      both sub-channels empty
#
# and the timing block at +0x800 reads tCL 5 against the real 18, with a tCWL
# of 6 against 17 -- low enough to drive the computed tWTR_L and tWTR_S rows
# negative, which is how it showed.
#
# Either way the ordinary timing registers hold the same values in both halves
# of a matched kit, so only a per-DIMM trained result tells the candidates
# apart: DFE taps on DDR5, RTL on DDR4.
CHANNEL_B_SUBCHANNEL_OFFSET = 0x800
CHANNEL_B_CONTROLLER_OFFSET = MCHBAR2 - MCHBAR


def channel_b_offset(platform=None):
    """Distance from a channel-A register to its channel-B twin."""
    if platform is None:
        platform = active_platform()
    if platform in DDR5_TIMING_PLATFORMS:
        return CHANNEL_B_SUBCHANNEL_OFFSET
    return CHANNEL_B_CONTROLLER_OFFSET


CHANNEL_B_OFFSET = channel_b_offset()
CHANNEL_B = MCHBAR + CHANNEL_B_OFFSET

# Timing rows whose name differs on DDR5: {declared name: DDR5 label}.
#
# DDR5 splits refresh into an all-bank interval and a same-bank one, named
# tRFC2 and tRFCpb; the register this row reads is the all-bank one, so tRFC2
# is what it is. DDR4 has a single tRFC and no per-bank refresh at all, which
# is why this is a rename per platform rather than a correction everywhere.
# tRFCns follows the same naming as the AM5 profile's row of that name.
#
# Applied last, by _install_ddr5_timing_labels at the foot of this module.
# Everything that matches rows by name -- the Arrow Lake field map, the
# secondary reorder, the tRFCns installer, the dual-channel promotion -- runs
# during import against the declared names, so the rename has to come after
# all of them. Only get_trfc_ns reads a row by name at render time, and it
# resolves the label the same way this does.
DDR5_TIMING_LABELS = {
    "tRFC": "tRFC2",
    "tRFC (ns)": "tRFCns",
    "tREFI (ns)": "tREFIns",
}


def ddr5_timing_label(platform, name):
    """The label a timing row carries on this platform. Pure, so it can be tested."""
    if platform in DDR5_TIMING_PLATFORMS:
        return DDR5_TIMING_LABELS.get(name, name)
    return name


@lru_cache(maxsize=None)
def _wmi_connection():
    """One WMI connection for the process.

    Building it is most of what a WMI lookup costs, and this file opened
    thirteen of them while the table was being built.
    """
    return wmi.WMI()


@lru_cache(maxsize=None)
def _wmi_static(class_name):
    """Return a WMI class's objects, queried once.

    Only for classes describing identity or installed hardware, which cannot
    change while the machine is running. Nine such classes were being queried
    eighteen times between them.

    Failure is cached too, deliberately: WMI being unavailable is not a
    transient condition, and retrying it once per caller is how the startup
    cost multiplied in the first place. dimm_inventory caches its own decode
    the same way.
    """
    try:
        return tuple(getattr(_wmi_connection(), class_name)())
    except Exception as e:
        print(f"Error querying {class_name}: {e}")
        return ()


# Intel client CPUID models, and the process node each is built on. Family 6
# throughout; the model is the extended CPUID byte.
#
# 0xB7 is what this bench reports and is confirmed against CPU-Z, which names
# it Raptor Lake on 10 nm. The rest are the other LGA1700 silicon this project
# targets. Anything unlisted prints its family and model rather than a guess,
# the same way an unlisted JEP106 vendor prints its raw ID.
INTEL_CODE_NAMES = {
    0x97: ("Alder Lake", "10 nm"),
    0x9A: ("Alder Lake", "10 nm"),
    0xBE: ("Alder Lake", "10 nm"),
    0xB7: ("Raptor Lake", "10 nm"),
    0xBA: ("Raptor Lake", "10 nm"),
    0xBF: ("Raptor Lake", "10 nm"),
}

# PCH LPC/eSPI controller device IDs at 00:1F.0. 0x7A04 is measured on this
# board, where CPU-Z names it Z790. Unlisted parts print the device ID.
PCH_DEVICE_NAMES = {
    0x7A04: "Z790",
}

# SMBIOS Type 17 memory types (SMBIOSMemoryType), which is where the DDR
# generation is stated. WMI's own MemoryType field reads 0 on this board.
#
# The low-power codes are here because detect_ddr_generation already treats
# them as their generation; without them an LPDDR5 machine printed "Type 35"
# in this row while the rest of the app called the same modules DDR5.
SMBIOS_MEMORY_TYPES = {
    20: "DDR", 21: "DDR2", 24: "DDR3",
    26: "DDR4", 30: "LPDDR4", 34: "DDR5", 35: "LPDDR5",
}


def _identity_cached(function):
    """Cache a zero-argument reading, treating a failure as no value.

    Same purpose as the lru_cache used elsewhere in this file, with one
    addition these need: a getter that raises -- no driver, no chip, no
    permission -- caches that as None rather than re-probing the hardware for
    every row that asks, on every draw.
    """
    return lru_cache(maxsize=None)(_swallowing(function))


def _swallowing(function):
    @wraps(function)
    def guarded(*args):
        try:
            return function(*args)
        except Exception:
            return None

    return guarded


@lru_cache(maxsize=None)
def _ecam_allocation():
    from intel_pch_smbus import default_ecam_allocation

    return default_ecam_allocation()


def _pci_config_dword(device, function, offset):
    """Read one dword from a PCI function's configuration space."""
    from pci_mcfg import ecam_address
    from read import read_physical_memory_int

    allocation = _ecam_allocation()
    if allocation is None:
        return None
    return read_physical_memory_int(
        ecam_address(allocation, 0, device, function, offset), 4
    )


def _clear_identity_caches():
    """Forget every cached platform reading. For tests and probes."""
    for cached in (_ecam_allocation, _intel_silicon, get_chipset_name,
                   get_southbridge_name, get_lpcio_name, get_memory_type,
                   get_os_name, _windows_version_values):
        cached.cache_clear()


def _cpu_family_model():
    # From system_identity, not from the probe that first defined it: that
    # module is a write-capable research tool and importing it here shipped
    # it inside the release EXE.
    from system_identity import decode_wmi_processor_id

    for cpu in _wmi_static("Win32_Processor"):
        return decode_wmi_processor_id(getattr(cpu, "ProcessorId", None))
    return None


@_identity_cached
def _intel_silicon():
    """Return ``(code name, process node)`` for this CPU.

    The node is a property of the silicon with nothing on the machine to read
    it from, so it is only reported for a part the table names: an unlisted
    one gets a code name built from its CPUID and no node at all, rather than
    inheriting its neighbour's.
    """
    found = _cpu_family_model()
    if found is None:
        return None, None
    family, model = found
    named = INTEL_CODE_NAMES.get(model) if family == 0x06 else None
    if named:
        return named
    return "Family %d Model 0x%02X" % (family, model), None


def get_cpu_codename():
    """The silicon's code name, from CPUID rather than the marketing string."""
    return (_intel_silicon() or (None, None))[0]


def get_cpu_technology():
    """The process node the silicon is built on."""
    return (_intel_silicon() or (None, None))[1]


def _pci_device_and_revision(device, function):
    """Return ``(device id, revision)`` for a PCI function, or ``(None, None)``."""
    identity = _pci_config_dword(device, function, 0x00)
    if identity is None or identity in (0xFFFFFFFF, 0x00000000):
        return None, None
    revision = (_pci_config_dword(device, function, 0x08) or 0) & 0xFF
    return (identity >> 16) & 0xFFFF, revision


@_identity_cached
def get_chipset_name():
    """The host bridge: the code name of the silicon, and its revision.

    CPU-Z calls this the Northbridge on its report and Chipset in its window.
    The name comes from CPUID -- on client parts the host bridge is the same
    die as the cores -- and the revision from 00:00.0, so both halves are
    read rather than assumed.
    """
    device, revision = _pci_device_and_revision(0x00, 0)
    if device is None:
        return None
    name = get_cpu_codename()
    if not name or name.startswith("Family "):
        name = "Host Bridge 0x%04X" % device
    return "Intel %s rev. %02X" % (name, revision)


@_identity_cached
def get_southbridge_name():
    """The PCH at 00:1F.0, by device ID, with its revision."""
    device, revision = _pci_device_and_revision(0x1F, 0)
    if device is None:
        return None
    return "Intel %s rev. %02X" % (
        PCH_DEVICE_NAMES.get(device, "PCH 0x%04X" % device), revision
    )


# Which vendor each sensor transport speaks for. The chip names the readers
# carry -- NCT6687D, NCT6798D, IT8696E -- do not say who makes them, and the
# row is read as "LPCIO" the way a board's spec sheet writes it.
LPCIO_VENDORS = {
    "superio_lpc": "Nuvoton",
    "nct679x": "Nuvoton",
    "ite_superio": "ITE",
}


@_identity_cached
def get_lpcio_name():
    """The Super I/O on the LPC bus, by its chip ID.

    Through the board sensors' own profile, which is the thing that already
    decided which chip is here. Probing separately would unlock and re-lock
    the configuration window under the monitoring mutex for an answer that
    cannot change -- and asking one reader by name gets it wrong: this row
    read nothing on the Z790-P, whose NCT6687D answers 0xD592 at port 0x4E.
    That is a Nuvoton, but not one the NCT679x reader knows, so the row went
    blank on a board whose Sensors tab was reading that very chip.

    The vendor comes from the reader that answered rather than from the chip
    name, which carries no vendor of its own.
    """
    from intel_board_sensors import board_sensor_profile

    profile = board_sensor_profile()
    reader = profile.get("reader") if profile else None
    name = getattr(reader, "chip_name", None)
    if not name:
        return None
    vendor = LPCIO_VENDORS.get(type(reader).__module__)
    return "%s %s" % (vendor, name) if vendor else name


@_identity_cached
def get_memory_type():
    """DDR generation, as SMBIOS states it."""
    found = []
    for memory in _wmi_static("Win32_PhysicalMemory"):
        code = getattr(memory, "SMBIOSMemoryType", None)
        if code is None:
            continue
        name = SMBIOS_MEMORY_TYPES.get(int(code), "Type %d" % int(code))
        if name not in found:
            found.append(name)
    return " / ".join(found) if found else None


def _wmi_live(class_name):
    """Query a class whose values move, reusing the shared connection.

    Performance counters belong here rather than in _wmi_static: caching one
    would pin a reading that is supposed to change.
    """
    try:
        return tuple(getattr(_wmi_connection(), class_name)())
    except Exception as e:
        print(f"Error querying {class_name}: {e}")
        return ()

# The clock rows are read down one column, so they have to be written the same
# way. Three of them were built by pasting the unit straight onto a rounded
# float, which put "5000.0Mhz" next to "2000 Mhz": no space, and a decimal
# point on a whole number. Rounding to a fixed number of places and then
# dropping what it added keeps a real fraction like 4987.5 and loses the ".0".
MHZ_DECIMALS = 3


def _mhz(value, decimals=MHZ_DECIMALS):
    """A megahertz reading, written the way the rest of the column is."""
    text = f"{round(float(value), decimals):.{decimals}f}".rstrip("0")
    return "%s Mhz" % text.rstrip(".")


def get_imc_freq():
    try:
        bclk = get_bclk()
        ratio = read_timing(MCHBAR + 0x5E04, bit_start=0, bit_length=8)
        raw_multiplier = read_timing(
            MCHBAR + 0x5E04, bit_start=8, bit_length=4
        )
        multiplier = {0: 4 / 3, 1: 1}.get(raw_multiplier)
        if not isinstance(bclk, (int, float)) or ratio is None or multiplier is None:
            return "Unknown"
        return _mhz(bclk * ratio * multiplier)
    except Exception:
        return "Error"

def get_bclk():
    try:
        return read_timing(MCHBAR + 0x5F60, bit_start=0, bit_length=32) / 1000
    except Exception:
        return "Error"

def get_bclk_rd():
    try:
        bclk = get_bclk()
        return _mhz(bclk) if isinstance(bclk, (int, float)) else bclk
    except Exception:
        return "Error"

def get_tx():
    try:
        tx = read_timing(MCHBAR + 0x5E00, bit_start=17, bit_length=10) / 200
        return f"{tx:.3f}V"
    except Exception:
        return "Error"

def get_sa():
    try:
        sa = read_timing(MCHBAR + 0x591C, bit_start=8, bit_length=23)/8192
        return f"{round(sa, 3)}V"
    except Exception:
        return "Error"
    
# Ring ratio sits at MCHBAR 0x5918[31:24] on Alder/Raptor Lake.  Core Ultra
# 200S keeps the identical field but relocates the block by 0x10000, so the
# legacy offset reads back 0 there.  Confirmed by diffing a full MCHBAR
# snapshot across a BIOS change: 0x15918[31:24] tracked 42 -> 39 exactly.
LEGACY_RING_RATIO = (0x5918, 24, 8)
ARROW_LAKE_RING_RATIO = (0x15918, 24, 8)

def get_ring_freq():
    """Ring/uncore clock: the ring ratio multiplied by BCLK."""
    try:
        offset, bit_start, bit_length = (
            ARROW_LAKE_RING_RATIO if is_arrow_lake_platform()
            else LEGACY_RING_RATIO
        )
        ratio = read_timing(
            MCHBAR + offset, bit_start=bit_start, bit_length=bit_length
        )
        bclk_khz = read_timing(MCHBAR + 0x5F60, bit_start=0, bit_length=32)
        if not ratio or not bclk_khz:
            return "N/A"
        return _mhz(ratio * bclk_khz / 1000)
    except Exception:
        return "Error"

def _get_cmd_stretch_raw(base=None):
    """Read the platform-native CMD_STRETCH encoding from SC_GS_CFG."""
    try:
        # Alder/Raptor Lake expose a two-bit 0..3 encoding. Arrow Lake changed
        # this to a single bit: 0 = 1N and 1 = 2N.
        bit_length = 1 if is_arrow_lake_platform() else 2
        value = read_timing(
            (MCHBAR if base is None else base) + 0xE088,
            bit_start=3,
            bit_length=bit_length,
            read_type="standard",
        )
        return None if value is None else int(value)
    except Exception:
        return None


def _get_n_to_1_ratio(base=None):
    """Read the extended N:1 ratio used by CMD_STRETCH encoding 3."""
    try:
        return read_timing(
            (MCHBAR if base is None else base) + 0xE088,
            bit_start=5,
            bit_length=3,
            read_type="standard",
        )
    except Exception:
        return None


def get_cmd_stretch(base=None):
    """Decode CMD Stretch instead of displaying its zero-based register value."""
    try:
        raw = _get_cmd_stretch_raw(base)
        if raw is None:
            return "Unknown"
        if is_arrow_lake_platform() or raw < 3:
            return raw + 1

        n_to_1_ratio = _get_n_to_1_ratio(base)
        return "N:Unknown" if n_to_1_ratio is None else f"N:{n_to_1_ratio}"
    except Exception:
        return "Error"


def get_command_rate(base=None):
    """Return the active command rate from the native CMD_STRETCH encoding."""
    try:
        raw = _get_cmd_stretch_raw(base)
        if raw is None:
            return "Unknown"

        if is_arrow_lake_platform():
            return "1N" if raw == 0 else "2N"

        if raw == 0:
            return "1N"
        elif raw == 1:
            return "2N"
        elif raw == 2:
            return "3N"
        elif raw == 3:
            n_to_1_ratio = _get_n_to_1_ratio(base)
            if n_to_1_ratio is None:
                return "N:Unknown"
            return f"N:{n_to_1_ratio}"
        return "Unknown"
    except Exception:
        return "Error"


def get_trc_value(base=None):
    """Return tRC as the active tRAS + tRP cycle total."""
    try:
        mc = MCHBAR if base is None else base
        tras_bit_start = 13 if is_arrow_lake_platform() else 10
        tras = read_timing(
            mc + 0xE004,
            bit_start=tras_bit_start,
            bit_length=9,
            read_type="standard",
        )
        trp = read_timing(
            mc + 0xE000,
            bit_start=0,
            bit_length=8,
            read_type="standard",
        )
        if tras is None or trp is None:
            return "Unknown"
        return tras + trp
    except Exception:
        return "Error"


def _get_twtr_value(bit_start, base=None):
    try:
        mc = MCHBAR if base is None else base
        turnaround = read_timing(
            mc + 0xE014,
            bit_start=bit_start,
            bit_length=7,
            read_type="standard",
        )
        tcwl = read_timing(
            mc + 0xE070,
            bit_start=24,
            bit_length=7,
            read_type="standard",
        )
        # DDR5 uses the same 10-clock controller-to-JEDEC conversion on both
        # Raptor Lake/Z790 and Arrow Lake/Z890. DDR4 uses six clocks.
        adjustment = 10 if (
            is_arrow_lake_platform() or detect_ddr_generation() == "DDR5"
        ) else 6
        return turnaround - tcwl - adjustment
    except Exception:
        return "Error"


def get_tWTR_L(base=None):
    """Return tWTR_L from the write-to-read same-bank-group timing."""
    return _get_twtr_value(0, base)


def get_tWTR_S(base=None):
    """Return tWTR_S from the write-to-read different-bank-group timing."""
    return _get_twtr_value(9, base)


# DDR5 splits every memory channel into two independent 32-bit sub-channels
# (JESD79-5), and it is the sub-channels the controller schedules against.
# This bench has DIMMs in channels A and B and trains four separate round-trip
# latencies for them -- MC0/MC1 x CHA/CHB read 70, 65, 71 and 65, with rank 1
# left at the unpopulated 25 on all four. Four sub-channels, one rank each.
#
# Both reference tools count those four: MemTweakIt shows "Channels 4" and
# ASRock's Timing Configurator "Channels # Quad", while listing the same two
# populated DIMM slots this does. Reporting the two DIMM channels instead
# disagreed with them and with this tool's own RTL block a tab away.
#
# DDR4 has no sub-channels, so there the count is the populated channels
# themselves and "Dual Channel" for two DIMMs stays right.
DDR5_SUBCHANNELS_PER_CHANNEL = 2

CHANNEL_LAYOUT_NAMES = {
    1: "Single Channel",
    2: "Dual Channel",
    3: "Triple Channel",
    4: "Quad Channel",
    6: "Six Channel",
    8: "Eight Channel",
}


def channel_layout_name(populated_channels, generation):
    """Name the layout for a count of populated channels. Pure, so it can be
    tested without a machine to read."""
    if not populated_channels:
        return None
    count = populated_channels
    if generation == "DDR5":
        count *= DDR5_SUBCHANNELS_PER_CHANNEL
    return CHANNEL_LAYOUT_NAMES.get(count, "%d Channels" % count)


def detect_dual_channel_memory():
    """How many channels the controller runs, named as the reference tools do.

    Counts the channels the installed modules populate, then doubles that on
    DDR5 for the sub-channels. See DDR5_SUBCHANNELS_PER_CHANNEL.

    Falls back to the slot-tag reading below when the module inventory is
    unavailable -- that path only ever knew about DIMM channels, so it answers
    DDR4's question rather than DDR5's, which is why it is the fallback and
    not the reading.
    """
    try:
        from dimm_inventory import read_modules

        channels = {
            module.get("channel") for module in read_modules()
            if module.get("channel")
        }
        named = channel_layout_name(len(channels), detect_ddr_generation())
        if named:
            return named
    except Exception:
        pass
    return _channel_layout_from_slot_tags()


def _channel_layout_from_slot_tags():
    try:
        # Get number of memory slots
        memory_arrays = _wmi_static("Win32_PhysicalMemoryArray")
        if not memory_arrays:
            return "No memory array detected"
        num_slots = memory_arrays[0].MemoryDevices

        # Get used memory slots
        used_slots = set(memory.Tag for memory in _wmi_static("Win32_PhysicalMemory"))

        if num_slots == 2:
            if {"Physical Memory 0", "Physical Memory 1"}.issubset(used_slots):
                return "Dual Channel"
            else:
                return "Single Channel"
        elif num_slots == 4:
            a_slots = {"Physical Memory 3", "Physical Memory 2"}
            b_slots = {"Physical Memory 1", "Physical Memory 0"}
            a_used = a_slots & used_slots
            b_used = b_slots & used_slots
            if a_used and b_used:
                return "Dual Channel"
            else:
                return "Single Channel"
        else:
            return f"{num_slots} DIMM slots detected - Unknown Channel Layout"
    except Exception as e:
        return f"Error detecting memory layout: {e}"
# Identity, not telemetry. None of the four functions below can return a
# different answer while the machine is running, and each costs a WMI query of
# roughly a second. Uncached, is_arrow_lake_platform alone was called 26 times
# while the table was being built, each time re-asking Windows for the CPU
# name, which is most of what made starting the viewer take half a minute.
@lru_cache(maxsize=None)
def get_cpu_name():
    try:
        cpu = _wmi_static("Win32_Processor")[0]
        cpu_name = cpu.Name.replace("Processor", "").strip()
        return cpu_name
    except Exception as e:
        print(f"Error retrieving CPU name: {e}")
        return "Error"
def get_total_physical_memory():
    try:
        computer_system = _wmi_static("Win32_ComputerSystem")[0]
        memory_bytes = int(computer_system.TotalPhysicalMemory)
        memory_gb = round(memory_bytes / (1024 ** 3))
        return f"{memory_gb}GB"
    except Exception as e:
        print(f"Error retrieving physical memory size: {e}")
        return "Error"
def get_motherboard_name():
    try:
        motherboard = _wmi_static("Win32_BaseBoard")[0]
        return motherboard.Product
    except Exception as e:
        print(f"Error retrieving motherboard name: {e}")
        return "Unknown"


def get_cpu_cores_threads():
    try:
        cpu = _wmi_static("Win32_Processor")[0]
        cores = getattr(cpu, "NumberOfCores", None)
        threads = getattr(cpu, "NumberOfLogicalProcessors", None)
        if cores and threads:
            return f"{cores} / {threads}"
        return "Unknown"
    except Exception as e:
        print(f"Error retrieving CPU cores/threads: {e}")
        return "Unknown"

@lru_cache(maxsize=None)
def get_motherboard_display():
    try:
        board = _wmi_static("Win32_BaseBoard")[0]
        manufacturer = (getattr(board, "Manufacturer", "") or "").strip()
        product = (getattr(board, "Product", "") or "").strip()
        version = (getattr(board, "Version", "") or "").strip()
        parts = [p for p in [manufacturer, product] if p]
        value = " ".join(parts).strip()
        if version and version not in value:
            value = f"{value} ({version})" if value else version
        return value or "Unknown"
    except Exception as e:
        print(f"Error retrieving motherboard display name: {e}")
        return "Unknown"

def _baseboard(attribute):
    """One stripped Win32_BaseBoard string, or "" when it is unavailable."""
    try:
        return (getattr(_wmi_static("Win32_BaseBoard")[0], attribute, "")
                or "").strip()
    except Exception as exc:
        print(f"Error retrieving motherboard {attribute}: {exc}")
        return ""


def _wmi_date(raw):
    """The date half of a WMI datetime as YYYY-MM-DD, or None.

    WMI datetime is yyyymmddHHMMSS.ffffff+UUU; only the date is meaningful for
    a firmware build or a driver release.
    """
    raw = str(raw or "")
    if len(raw) < 8 or not raw[:8].isdigit():
        return None
    return "%s-%s-%s" % (raw[0:4], raw[4:6], raw[6:8])


def get_board_manufacturer():
    """Who made the board, as SMBIOS states it."""
    return _baseboard("Manufacturer") or "Unknown"


def get_board_model():
    """The board itself, with the revision SMBIOS carries beside it."""
    product = _baseboard("Product")
    version = _baseboard("Version")
    if version and version not in product:
        return ("%s (%s)" % (product, version)).strip()
    return product or "Unknown"


# The card, in the order CPU-Z's Graphics tab reads it: what board it is, what
# silicon is on it, then the frame buffer attached to that silicon. Names are
# prefixed where the Processor section already owns the word.
GPU_ROWS = (
    ("Board Manufacturer", "board_manufacturer"),
    ("GPU Code Name", "code_name"),
    ("GPU Revision", "revision"),
    ("Cores", "cores"),
    ("ROPs / TMUs", "rops_tmus"),
    ("GPU Technology", "technology"),
    ("Memory Size", "memory_size"),
    ("Memory Type", "memory_type"),
    ("Memory Vendor", "memory_vendor"),
    ("Bus Width", "bus_width"),
    ("Resizable BAR", "resizable_bar"),
    ("Driver Version", "driver_version"),
)


def _real_display_adapters():
    """The installed display adapters, skipping Windows' fallback driver.

    The Basic Display Adapter is what Windows installs before the vendor
    driver; it is not the card in the machine.
    """
    for adapter in _wmi_static("Win32_VideoController"):
        name = (getattr(adapter, "Name", "") or "").strip()
        if name and "Basic Display" not in name:
            yield adapter


def _gpu_field(field):
    """One value from the graphics card, or None if it did not answer.

    The adapter ids come from the WMI class this module has already queried
    and cached, so the card module needs no connection of its own.
    """
    try:
        from nvidia_gpu import read_gpu

        return read_gpu(pnp_device_ids=[
            getattr(adapter, "PNPDeviceID", "")
            for adapter in _real_display_adapters()
        ]).get(field)
    except Exception:
        return None


def get_bios_version():
    try:
        bios = _wmi_static("Win32_BIOS")[0]
        version = (getattr(bios, "SMBIOSBIOSVersion", "") or getattr(bios, "Version", "") or "").strip()
        return version or "Unknown"
    except Exception as e:
        print(f"Error retrieving BIOS version: {e}")
        return "Unknown"

def get_microcode():
    """Return the actual microcode revision, not the packed registry QWORD."""
    try:
        registry_path = r"HARDWARE\DESCRIPTION\System\CentralProcessor\0"
        with winreg.OpenKey(winreg.HKEY_LOCAL_MACHINE, registry_path) as key:
            value, _ = winreg.QueryValueEx(key, "Update Revision")
        if isinstance(value, bytes):
            microcode = int.from_bytes(value, byteorder="little", signed=False)
        elif isinstance(value, int):
            microcode = value
        else:
            return "Unknown"

        # Windows commonly stores the revision in the upper 32 bits of the
        # Update Revision value. For example 0x13300000000 means revision 0x133.
        if microcode > 0xFFFFFFFF:
            upper = (microcode >> 32) & 0xFFFFFFFF
            lower = microcode & 0xFFFFFFFF
            microcode = upper if upper else lower

        return f"0x{microcode:X}"
    except Exception as e:
        print(f"Error retrieving microcode revision: {e}")
        return "Unknown"

def get_ram_manufacturer():
    try:
        manufacturers = []
        for mem in _wmi_static("Win32_PhysicalMemory"):
            name = (getattr(mem, "Manufacturer", "") or "").strip()
            if name and name not in manufacturers:
                manufacturers.append(name)
        return ", ".join(manufacturers) if manufacturers else "Unknown"
    except Exception as e:
        print(f"Error retrieving RAM manufacturer: {e}")
        return "Unknown"

def _parse_first_number(value):
    try:
        if value is None:
            return None
        text = str(value).replace(",", "")
        num = ""
        allowed = set("0123456789.-")
        started = False
        for ch in text:
            if ch in allowed:
                num += ch
                started = True
            elif started:
                break
        return float(num) if num else None
    except Exception:
        return None

@lru_cache(maxsize=None)
def is_arrow_lake_platform():
    """Detect Core Ultra 200S/Z890-style memory-controller register layout."""
    try:
        cpu_name = get_cpu_name().upper()
        board_name = get_motherboard_display().upper()
        qclk_ratio = read_timing(
            MCHBAR + 0x13D10,
            bit_start=0,
            bit_length=8,
            read_type="standard",
        )
        register_is_valid = qclk_ratio is not None and 1 <= int(qclk_ratio) <= 254
        return register_is_valid and (
            "Z890" in board_name
            or "CORE(TM) ULTRA" in cpu_name
            or "CORE ULTRA" in cpu_name
        )
    except Exception as e:
        print(f"Error detecting Arrow Lake platform: {e}")
        return False


def get_dram_ratio_value():
    try:
        if is_arrow_lake_platform():
            ratio = read_timing(
                MCHBAR + 0x13D10,
                bit_start=0,
                bit_length=8,
                read_type="standard",
            )
        else:
            ratio = read_timing(
                MCHBAR + 0x5E04,
                bit_start=0,
                bit_length=8,
                read_type="standard",
            )
        return str(int(ratio)) if ratio is not None else "Unknown"
    except Exception as e:
        print(f"Error retrieving DRAM ratio: {e}")
        return "Unknown"


def get_gear_mode_value():
    try:
        if is_arrow_lake_platform():
            raw_gear = read_timing(
                MCHBAR + 0x13D10,
                bit_start=8,
                bit_length=1,
                read_type="standard",
            )
            return "Gear Mode 2" if raw_gear == 0 else "Gear Mode 4"

        raw_gear = read_timing(
            MCHBAR + 0x5E04,
            bit_start=12,
            bit_length=2,
            read_type="standard",
        )
        return GEAR_MODE_FORMULA.get(int(raw_gear), "Unknown") if raw_gear is not None else "Unknown"
    except Exception as e:
        print(f"Error retrieving gear mode: {e}")
        return "Unknown"


def get_dram_frequency():
    try:
        freq = _parse_first_number(get_speed())
        if freq is None:
            return "Unknown"
        return f"{freq:.0f} Mhz"
    except Exception as e:
        print(f"Error retrieving DRAM frequency: {e}")
        return "Unknown"

def get_mclk():
    try:
        freq = _parse_first_number(get_speed())
        if freq is None:
            return "Unknown"
        mclk = freq / 2
        return f"{mclk:.0f} Mhz"
    except Exception as e:
        print(f"Error retrieving MCLK: {e}")
        return "Unknown"

# The memory controller clock runs at MCLK divided by the active gear ratio,
# so Gear 2 at 4400 MCLK gives a 2200 UCLK.  Returning MCLK unchanged made
# every geared configuration report UCLK too high.
GEAR_DIVIDER = {
    "Gear Mode 1": 1,
    "Gear Mode 2": 2,
    "Gear Mode 4": 4,
}

def get_uclk():
    try:
        mclk = _parse_first_number(get_mclk())
        divider = GEAR_DIVIDER.get(str(get_gear_mode_value()))
        if mclk is None or divider is None:
            return "Unknown"
        return f"{mclk / divider:.0f} Mhz"
    except Exception as e:
        print(f"Error retrieving UCLK: {e}")
        return "Unknown"

def _read_finalized_timing(name, base=None):
    """Read a timing row by name after the platform installers set its address.

    A row's stored address always names the first memory controller, so ``base``
    reads the same offset on another one. An address that does not sit inside
    MC0's window has no known twin, so that request returns None rather than a
    register that was never confirmed to hold this timing.
    """
    for timing in TIMINGS:
        if timing.get("name") != name:
            continue
        address = timing.get("address")
        params = timing.get("parameters") or {}
        if address is None or "bit_start" not in params:
            continue
        if base is not None:
            if not MCHBAR <= address < MCHBAR2:
                return None
            address = base + (address - MCHBAR)
        return read_timing(
            address=address,
            bit_start=params["bit_start"],
            bit_length=params["bit_length"],
            read_type="standard",
        )
    return None

def get_trfc_ns(base=None):
    """Convert the live refresh intervals into nanoseconds using the MCLK.

    On DDR5 the row carries both: the all-bank interval the controller
    refreshes on, then the same-bank one, which is the pair a tuner compares
    and which no single figure can show. DDR4 has no per-bank refresh, so it
    stays one number there.

    MCLK is a controller-wide clock, so only the interval reads are
    per-channel.
    """
    try:
        mclk = _parse_first_number(get_mclk())
        if not mclk:
            return None
        platform = active_platform()
        all_bank = _read_finalized_timing(
            ddr5_timing_label(platform, "tRFC"), base
        )
        if not all_bank:
            return None
        text = f"{all_bank * 1000.0 / mclk:.0f} ns"
        if platform not in DDR5_TIMING_PLATFORMS:
            return text
        per_bank = _read_finalized_timing("tRFCpb", base)
        if not per_bank:
            # The all-bank interval on its own beats nothing, and a board that
            # does not report the per-bank one is not an error. It keeps its
            # unit, because on its own the row's "(ns)" is naming a pair that
            # is not there.
            return text
        # The all-bank interval then the per-bank one, with the unit named
        # once at the end: "120/98 (ns)", which is how the AM5 profile shows
        # the same pair.
        return "%.0f/%.0f (ns)" % (all_bank * 1000.0 / mclk,
                                   per_bank * 1000.0 / mclk)
    except Exception as e:
        print(f"Error retrieving tRFCns: {e}")
        return None

def get_trefi_ns(base=None):
    """tREFI restated as the time it actually is.

    tREFI is how long the controller may go between refreshes, and a count of
    memory clocks means nothing without the clock: 65535 cycles at 4000 MHz is
    16384 ns. Whole nanoseconds, matching the tRFCns beside it -- a refresh
    window is not tuned to a picosecond, and the hundredths would come from
    the division rather than from any precision the reading has.
    """
    try:
        mclk = _parse_first_number(get_mclk())
        if not mclk:
            return None
        cycles = _read_finalized_timing("tREFI", base)
        if not cycles:
            return None
        return f"{cycles * 1000.0 / mclk:.0f} ns"
    except Exception as e:
        print(f"Error retrieving tREFIns: {e}")
        return None


def get_qclk_ratio():
    try:
        if is_arrow_lake_platform():
            return "33.33 Mhz"

        rawmul = read_timing(MCHBAR + 0x5E04, bit_start=8, bit_length=4)
        if rawmul == 0:
            return "133.33 Mhz"
        if rawmul == 1:
            return "100.00 Mhz"
        return str(rawmul)
    except Exception as e:
        print(f"Error retrieving DDR QCLK ratio: {e}")
        return "Unknown"

def get_psf0_pll():
    try:
        bclk = get_bclk()
        if isinstance(bclk, (int, float)):
            return f"{(bclk * 10.6666):.2f} Mhz"
        return "Unknown"
    except Exception as e:
        print(f"Error retrieving PSF0 PLL: {e}")
        return "Unknown"


@lru_cache(maxsize=None)
def detect_ddr_generation():
    """Return DDR4, DDR5, or Unknown using SMBIOS/WMI with board-name fallbacks."""
    try:
        detected = []
        for memory in _wmi_static("Win32_PhysicalMemory"):
            for field in ("SMBIOSMemoryType", "MemoryType"):
                raw = getattr(memory, field, None)
                try:
                    code = int(raw)
                except (TypeError, ValueError):
                    continue
                # SMBIOS type codes: DDR4/LPDDR4 and DDR5/LPDDR5.
                if code in (26, 30):
                    detected.append("DDR4")
                elif code in (34, 35):
                    detected.append("DDR5")

        if "DDR5" in detected:
            return "DDR5"
        if "DDR4" in detected:
            return "DDR4"

        # Some systems do not populate SMBIOSMemoryType correctly. Check text fields.
        text_fields = []
        for memory in _wmi_static("Win32_PhysicalMemory"):
            for field in ("PartNumber", "Description", "Caption"):
                value = (getattr(memory, field, "") or "").upper()
                if value:
                    text_fields.append(value)
        for board in _wmi_static("Win32_BaseBoard"):
            text_fields.extend([
                (getattr(board, "Product", "") or "").upper(),
                (getattr(board, "Version", "") or "").upper(),
            ])
        joined = " ".join(text_fields)
        if "DDR5" in joined:
            return "DDR5"
        if "DDR4" in joined:
            return "DDR4"
    except Exception as e:
        print(f"Error detecting DDR generation: {e}")
    return "Unknown"


def _get_twr_from_controller(ddr_generation=None, base=None):
    """Derive active tWR from the platform-specific write-precharge and tCWL fields."""
    try:
        if ddr_generation is None:
            ddr_generation = detect_ddr_generation()
        mc = MCHBAR if base is None else base

        if is_arrow_lake_platform():
            # Core Ultra 200S: tWRPRE is TC_PRE[42:33].
            twr_pre = read_timing(
                mc + 0xE004,
                bit_start=1,
                bit_length=10,
                read_type="standard",
            )
            tcwl = read_timing(
                mc + 0xE070,
                bit_start=24,
                bit_length=8,
                read_type="standard",
            )
            if twr_pre is None or tcwl is None:
                return None
            # DDR5 BL16 occupies 8 memory clocks. TC_PRE.tWRPRE starts at
            # the WRITE command, while tWR begins after the burst.
            value = int(twr_pre) - int(tcwl) - 8
            return value if value > 0 else None

        twr_pre = read_timing(
            mc + 0xE004,
            bit_start=0,
            bit_length=10,
            read_type="standard",
        )
        tcwl = read_timing(
            mc + 0xE070,
            bit_start=24,
            bit_length=7,
            read_type="standard",
        )
        if twr_pre is None or tcwl is None:
            return None

        # On Intel DDR4, TC_PRE.tWRPRE is the full WR-command-to-PRE delay.
        # DDR4 tWR begins after the BL8 write burst, so remove tCWL plus
        # BL/2 (8 transfers / 2 = 4 clocks) to obtain the value shown by
        # runtime timing tools such as HWiNFO.
        if ddr_generation == "DDR4":
            value = int(twr_pre) - int(tcwl) - 4
        elif ddr_generation == "DDR5":
            value = int(twr_pre) - int(tcwl) - 8
        else:
            value = int(twr_pre) - int(tcwl)
        return value if value > 0 else None
    except Exception as e:
        print(f"Error deriving tWR from controller registers: {e}")
        return None

def _get_twr_ddr5_mr_fallback(base=None):
    """Fallback for DDR5 systems whose controller-derived tWR is unavailable."""
    try:
        for offset_start in (0xE600, 0xE000):
            for cmd in (0, 1, 2, 3):
                raw_value = read_timing(
                    read_type="dynamic",
                    dynamic_params={
                        "offset_start": offset_start,
                        "value_to_find": 0x06,
                        "offset_base": 0xE200,
                        "bit_start_dynamic": 0,
                        "bit_length_dynamic": 4,
                        "mchbar": MCHBAR if base is None else base,
                        "command": cmd,
                        "offset2": 0,
                    },
                )
                if raw_value is not None:
                    mapped = apply_formula(int(raw_value) & 0xF, tWR_FORMULA)
                    if mapped != "N/A":
                        return mapped
    except Exception as e:
        print(f"Error reading DDR5 tWR fallback: {e}")
    return None


def get_twr_value(base=None):
    """Auto-detect DDR4/DDR5 and return the active tWR timing."""
    ddr_generation = detect_ddr_generation()
    controller_value = _get_twr_from_controller(ddr_generation, base)

    # The controller-derived value is the configured timing and works for both DDR4 and DDR5.
    if controller_value is not None:
        if ddr_generation == "DDR4" and 4 <= controller_value <= 64:
            return controller_value
        if ddr_generation == "DDR5" and 16 <= controller_value <= 192:
            return controller_value
        if ddr_generation == "Unknown" and 4 <= controller_value <= 192:
            return controller_value

    # Only use the existing MR-table decoder on DDR5. It is not a DDR4 decoder.
    if ddr_generation == "DDR5":
        mr_value = _get_twr_ddr5_mr_fallback(base)
        if mr_value is not None:
            return mr_value

    return "Unknown"


# MCHBAR 0xE440[31:24] is the Alder/Raptor Lake tMOD field and reads back 0
# on Core Ultra 200S.  Three BIOS values (8, 31, 20) were captured as full
# MCHBAR snapshots: no field anywhere in 0x0-0x20000 tracks the setting, as
# a raw value, with a constant offset, or scaled.  Report N/A rather than a
# coincidental match -- 0x0E430 and 0x079C both matched one transition and
# failed the next.
LEGACY_TMOD = (0xE440, 24, 8)

def get_tmod_value():
    """Return the live tMOD value from the memory controller."""
    try:
        offset, bit_start, bit_length = LEGACY_TMOD
        value = read_timing(
            MCHBAR + offset,
            bit_start=bit_start,
            bit_length=bit_length,
            read_type="standard",
        )
        if value is None or int(value) == 0:
            return "N/A"
        return int(value)
    except Exception as e:
        print(f"Error retrieving tMOD: {e}")
        return "Error"


# Column-to-column delays, from Intel's documented memory-controller fields.
# TC_RDRD holds the read-to-read pair and TC_WRWR the write-to-write one; in
# each, field .sg is the same-bank-group case and .dg the different-bank-group
# one. Both are already in tCK and both exist on DDR4 and DDR5.
TC_RDRD_OFFSET = 0xE00C
TC_WRWR_OFFSET = 0xE018
BANK_GROUP_SAME_BIT = 0
BANK_GROUP_DIFFERENT_BIT = 8


def _get_bank_group_timing(setup_name, register_offset, bit_start, base=None):
    """Return a live column-to-column timing in DRAM tCK cycles."""
    try:
        value = read_timing(
            (MCHBAR if base is None else base) + register_offset,
            bit_start=bit_start,
            bit_length=7,
            read_type="standard",
        )
        if value is None:
            return "Unknown"
        value = int(value)
        return value if 1 <= value <= 127 else "Unknown"
    except Exception as e:
        print(f"Error retrieving {setup_name}: {e}")
        return "Error"


# timing name -> (register offset, bit start).
#
# Empty pending hardware confirmation.
#
# TC_RDRD.sg and TC_WRWR.sg were used here, on the strength of the controller
# reading 8 and 4 while the BIOS reported tCCD_L 8 and tCCD 4 with both on
# Auto. A controlled change refuted that: setting tCCD 6 and tCCD_L 10 and
# rebooting left TC_RDRD at 0x08080488 and TC_WRWR at 0x08080408, byte for
# byte, on both controllers. Two defaults had coincided with two register
# fields, which is not the same as the fields carrying those settings.
#
# The scheduler fields are still read and still correct as what they are: they
# appear on the Timings tab as tRDRD_sg, tRDRD_dg and tWRWR_sg, which is the delay
# the controller actually applies. Only the claim that they *are* tCCD and
# tCCD_L was wrong, so only that claim was withdrawn.
CCD_CONFIRMED_FIELDS = {}

# No candidate either, and the search is finished rather than merely stalled.
#
# 0xE08C looked convincing at tCCD 6 / tCCD_L 10: it read 0x04008A06 on both
# controllers, putting 6 at bits 0-6 and 10 at bits 8-14, and it was the only
# register in the swept window holding both values. A second controlled change
# to tCCD 8 / tCCD_L 14 left it at 0x04008A06, byte for byte. It had simply
# always held those numbers.
#
# That second change also settled the wider question. Diffing full snapshots of
# both controllers across the reboot, 7 registers of 768 differed: 0xE034 and
# 0xE040 drift between back-to-back reads at fixed settings, so they are live
# counters rather than configuration, and 0xE474 and 0xE4B0 are stable within a
# boot but carry no field moving 6->8 or 10->14, which is what per-boot
# training variance looks like. Sweeping the whole 0x0000-0x10000 window on
# both controllers found no register anywhere holding 8 and 14 in byte-aligned
# fields.
#
# The physical argument agrees: TC_RDRD.sg is the delay the controller applies
# between same-bank-group reads, and it stayed at 8 across both changes. It
# could not, had the DRAM actually been running tCCD_L 14.
#
# So this board's BIOS accepts the setting without programming it into the
# memory controller, and there is nothing to read. These rows report nothing
# because nothing carries the value, not because the search was abandoned.
CCD_CANDIDATE_FIELDS = {}


# --- DDR5 mode-register shadow.
#
# The controller keeps the mode registers it programmed into the DIMMs in a
# table here, one dword each: the MR number in bits 8-15 and the value it
# wrote in bits 0-7. Confirmed on the Z790 bench, where the table runs MR0
# upward from 0xE600 and both controllers hold identical copies.
#
# This is where tCCD_L lives, and why the earlier sweep for it failed. That
# search swept both controllers for a field holding the cycle count across a
# controlled BIOS change. The DRAM does not store a cycle count: MR13 holds a
# 4-bit JEDEC code. A search for the count could never have found the code,
# so the conclusion that nothing carried the value came from searching for
# the wrong representation rather than from the value being absent.
MODE_REGISTER_TABLE_START = 0xE600
MODE_REGISTER_TABLE_END = 0xE800
MODE_REGISTER_ENTRY_PREFIX = 0x8000
MODE_REGISTER_PAYLOAD_BASE = 0xE200


def read_mode_register(number, base=None):
    """Return one DDR5 mode register's value, or None when it is not listed.

    An entry does not hold the value. Its data byte names the mode register
    and its index byte is an offset into the payload array at 0xE200, which
    is where the contents are. This read matched on the index instead and
    returned the data byte -- for MR13 that gave 2 where the register holds
    8, and tCCD_L came out 10 rather than 16.

    Two independent checks say this is the right way round. MR0 resolves to
    0x1C, which decodes to BL16 and CL 36, and 36 is exactly the tCL read
    from the timing registers. And the reference map spells the lookup out:
    the tCCD block is written as search_value "0D" with base_address
    (mchbar)E200, which is a search on the data byte followed by the index.
    """
    start = MCHBAR if base is None else base
    try:
        for offset in range(MODE_REGISTER_TABLE_START,
                            MODE_REGISTER_TABLE_END, 4):
            entry = read_timing(
                address=start + offset, bit_start=0, bit_length=32,
                read_type="standard",
            )
            if entry is None:
                continue
            entry &= 0xFFFFFFFF
            if (entry >> 16) & 0xFFFF != MODE_REGISTER_ENTRY_PREFIX:
                continue
            if entry & 0xFF != int(number) & 0xFF:
                continue
            payload = read_timing(
                address=start + MODE_REGISTER_PAYLOAD_BASE
                + ((entry >> 8) & 0xFF),
                bit_start=0, bit_length=8, read_type="standard",
            )
            return None if payload is None else int(payload) & 0xFF
    except Exception as exc:
        print(f"Error reading mode register {number}: {exc}")
    return None


# MR13 carries tCCD_L in its low nibble, and the write and second-write
# variants are fixed multiples of it. JESD79-5 encodes them as:
#
#   tCCD_L      = 8  + code   (8 to 22 nCK)
#   tCCD_L_WR   = 16 + 2*code
#   tCCD_L_WR2  = 32 + 4*code
#
# Confirmed on the bench: MR13 reads 0x02 on both controllers, giving 10 / 20
# / 40. That agrees with the controller's own applied delay -- TC_RDRD.sg is
# 16, which has to be at least tCCD_L and could not be if tCCD_L were 20.
# Code 15 is reserved rather than a timing, so it reports nothing.
# --- DFE, from the same mode-register table.
#
# The table's entries carry a data byte, a pointer and a command code. A DFE
# tap is found by its data byte -- 0x81 through 0x84 for taps 1 to 4 -- and
# the entry then points at a byte holding all three of its settings: enable at
# bit 7, sign at bit 6, magnitude in bits 0-5.
#
# Read together rather than as three lookups. A tap's bias is a signed number
# that happens to be stored as two fields, and searching the table three times
# to reassemble one value would also let the sign and the magnitude come from
# different reads.
DFE_TAP_DATA_BASE = 0x80
DFE_TAP_COUNT = 4
DFE_ENABLE_BIT = 7
DFE_SIGN_BIT = 6
DFE_BIAS_BITS = 6


_MODE_REGISTER_TABLE_POPULATED = {}


def _mode_register_table_populated(start):
    """True when the table has been filled in at all.

    A platform that never programs it -- DDR4 here -- leaves all 128 entries
    reading zero, and a zero entry is a perfect match for register 0 with
    command 0 pointing at payload index 0. So a search for MR0 succeeds
    against a table that holds nothing, and Burst Length reported BL16: a
    code DDR4 does not have, read out of an empty register. Every other Misc
    row asks for a non-zero register and correctly found nothing, which is
    why that one row was the only one that lied.

    Cached, but only once something could actually be read -- called before
    the driver is up it would otherwise cache "empty" for the session.
    """
    cached = _MODE_REGISTER_TABLE_POPULATED.get(start)
    if cached is not None:
        return cached
    readable = False
    for offset in range(MODE_REGISTER_TABLE_START,
                        MODE_REGISTER_TABLE_END, 4):
        value = read_timing(
            address=start + offset, bit_start=0, bit_length=32,
            read_type="standard",
        )
        if value is None:
            continue
        readable = True
        if value & 0xFFFFFFFF:
            _MODE_REGISTER_TABLE_POPULATED[start] = True
            return True
    if readable:
        _MODE_REGISTER_TABLE_POPULATED[start] = False
    return False


def _mode_register_pointer(data_byte, command=0, offset_base=0xE200, base=None):
    """Find a table entry by its data byte and return where it points."""
    start = MCHBAR if base is None else base
    if not _mode_register_table_populated(start):
        return None
    for offset in range(MODE_REGISTER_TABLE_START,
                        MODE_REGISTER_TABLE_END, 4):
        value = read_timing(
            address=start + offset, bit_start=0, bit_length=32,
            read_type="standard",
        )
        if value is None:
            continue
        value &= 0xFFFFFFFF
        if value & 0xFF != (data_byte & 0xFF):
            continue
        if (value >> 22) & 0x3 != command:
            continue
        return start + offset_base + ((value >> 8) & 0xFF)
    return None


def _read_dfe_tap(tap, base=None):
    """Return ``(enabled, bias)`` for one DFE tap, or None."""
    try:
        address = _mode_register_pointer(DFE_TAP_DATA_BASE + int(tap), base=base)
        if address is None:
            return None
        raw = read_timing(
            address=address, bit_start=0, bit_length=8, read_type="standard",
        )
        if raw is None:
            return None
        raw &= 0xFF
        magnitude = raw & ((1 << DFE_BIAS_BITS) - 1)
        bias = -magnitude if (raw >> DFE_SIGN_BIT) & 1 else magnitude
        return bool((raw >> DFE_ENABLE_BIT) & 1), bias
    except Exception as exc:
        print(f"Error reading DFE tap {tap}: {exc}")
        return None


def get_dfe_enable(tap, base=None):
    """"Enable" or "Disable" for one tap, or None when the table has no entry."""
    reading = _read_dfe_tap(tap, base)
    return None if reading is None else ("Enable" if reading[0] else "Disable")


def get_dfe_bias(tap, base=None):
    """The tap's signed bias, written the way a sign-and-magnitude field reads."""
    reading = _read_dfe_tap(tap, base)
    return None if reading is None else "%+d" % reading[1]


# --- DDR4 mode-register shadow.
#
# DDR4 never fills the 0xE600 table, so every row that reads a mode register
# through it reports nothing. The registers are not missing, though: the
# controller keeps what it last wrote to the DRAM at MCHBAR + 0xE5A0, eight
# registers packed two to a dword, low half first.
#
# Found by following 0xE5AC -- the register the DDR4 DQ VREF row already read
# -- and asking whether its neighbours decode as MR0 through MR7. Four checks
# say they do, each against a number that comes from somewhere else entirely:
#
#   MR0 CL code 8       -> CL 18, and the controller's tCL register reads 18
#   MR0 WR/RTP code 6   -> WR 24, and the tWR row reads 24
#   MR3 FGR code 0      -> normal refresh, which is what Refresh Mode shows
#   MR6 tCCD_L code 4   -> 8, which is what the BIOS shows for tCCD_L
#
# and the two controllers agree on every register except MR6, where they read
# 0x101C against 0x101B: a per-DIMM trained VrefDQ. That is the one field that
# should differ between two sticks, and nothing a static copy could produce.
#
# MR4 is deliberately not read here. Only bit 3 is set on this bench, which
# is not enough to tell which field it is, so the three preamble rows keep
# reporting nothing until a controlled BIOS change says which bit moves.
DDR4_MODE_REGISTER_BASE = 0xE5A0
DDR4_MODE_REGISTER_COUNT = 8


def _ddr4_mode_register(number, base=None):
    """One DDR4 mode register out of the shadow, or None."""
    if not 0 <= number < DDR4_MODE_REGISTER_COUNT:
        return None
    start = MCHBAR if base is None else base
    offset = DDR4_MODE_REGISTER_BASE + (number // 2) * 4
    try:
        raw = read_physical_memory_int(start + offset, 4)
    except Exception as exc:
        print(f"Error reading DDR4 mode register {number}: {exc}")
        return None
    if raw is None:
        return None
    raw = int(raw) & 0xFFFFFFFF
    if raw == 0xFFFFFFFF:
        return None
    return (raw >> (16 * (number % 2))) & 0xFFFF


def _ddr4_mode_register_field(number, bit_start, bit_length, base=None):
    """One field out of the DDR4 shadow, or None when it cannot be read."""
    value = _ddr4_mode_register(number, base)
    if value is None:
        return None
    return (value >> bit_start) & ((1 << bit_length) - 1)


# MR6 A[12:10]. Codes above 4 are not defined.
DDR4_TCCD_L = {0: 4, 1: 5, 2: 6, 3: 7, 4: 8}
DDR4_TCCD_L_MODE_REGISTER = 0x06
DDR4_TCCD_L_BITS = (10, 3)


def _ddr4_ccd_l(base=None):
    """tCCD_L out of the DDR4 MR6 shadow, or None."""
    code = _ddr4_mode_register_field(
        DDR4_TCCD_L_MODE_REGISTER, *DDR4_TCCD_L_BITS, base=base)
    return None if code is None else DDR4_TCCD_L.get(code)


CCD_MODE_REGISTER = 0x0D
CCD_CODE_RESERVED = 0x0F
CCD_FROM_MR13 = {
    "tCCD_L": (8, 1),
    "tCCD_L_WR": (16, 2),
    "tCCD_L_WR2": (32, 4),
}


# tDLLK comes from the same MR13 nibble tCCD_L does, under a different
# formula: the lock time steps every second code rather than every code, so
# 8 and 9 both give 2048. Taken from the reference tools' table -- 1024,
# 1024, 1280, 1280 ... 2816 -- which is 1024 + 256 per pair, with the
# reserved code at the top shared with tCCD_L.
DLLK_BASE_CYCLES = 1024
DLLK_STEP_CYCLES = 256


def get_dllk_timing(base=None):
    """DLL lock time in cycles, or None when MR13 holds the reserved code."""
    try:
        raw = read_mode_register(CCD_MODE_REGISTER, base)
        if raw is None:
            return None
        code = int(raw) & 0x0F
        if code == CCD_CODE_RESERVED:
            return None
        return DLLK_BASE_CYCLES + DLLK_STEP_CYCLES * (code // 2)
    except Exception as exc:
        print(f"Error reading tDLLK: {exc}")
        return None


def get_ccd_timing(name, base=None):
    """Return a column-to-column timing, or None when nothing carries it."""
    # DDR4 carries tCCD_L in MR6 rather than MR13, and has no register for
    # the write variants at all -- those two stay blank rather than being
    # derived from a DDR5 formula that has nothing behind it here.
    if detect_ddr_generation() == "DDR4":
        return _ddr4_ccd_l(base) if name == "tCCD_L" else None

    derived = CCD_FROM_MR13.get(name)
    if derived is not None:
        raw = read_mode_register(CCD_MODE_REGISTER, base)
        if raw is None:
            return None
        code = raw & 0x0F
        if code == CCD_CODE_RESERVED:
            return None
        offset, step = derived
        return offset + step * code

    field = CCD_CONFIRMED_FIELDS.get(name)
    if field is None:
        return None
    offset, bit_start = field
    value = _get_bank_group_timing(name, offset, bit_start, base)
    return None if value in ("Unknown", "Error") else value

MULTIPLIER_FORMULA = {
    0: "133.33",
    1: "100",
}

DFE_ENABLE_FORMULA = {
    0: "ON",
    1: "OFF",
}
DFE_TAP_ENABLE_FORMULA = {
    0: "OFF",
    1: "ON",
}
DFE_GAIN_FORMULA ={
    0: "0",
    1: "1",
    2: "2",
    3: "3",
    4: "RFU",
    5: "RFU",
    6: "RFU",
    7: "RFU",
    8: "-0",
    9: "-1",
    10: "-2",
    11: "-3",
    12: "RFU",
    13: "RFU",
    14: "RFU",
    15: "RFU"
}

DFE_TAP_FORMULA ={
    0: "RZQ/7 (34)",
    1: "RZQ/6 (40)",
    2: "RZQ/5 (48)",
    3: "RFU",
    4: "RZQ/7 (34)",
    5: "RZQ/6 (40)",
    6: "RZQ/5 (48)",
    7: "RFU",
    8: "RZQ/7 (34)",
    9: "RZQ/6 (40)",
    10: "RZQ/5 (48)",
    11: "RFU",
    12: "RZQ/7 (34)",
    13: "RZQ/6 (40)",
    14: "RZQ/5 (48)",
    15: "RFU"
}

RON_FORMULA ={
    0: "RZQ/7 (34)",
    1: "RZQ/6 (40)",
    2: "RZQ/5 (48)",
    3: "RFU"
}
GEAR_MODE_FORMULA = {
    0: "Gear Mode 1",
    1: "Gear Mode 2",
    2: "Gear Mode 4"
}
REFRESH_MODE_FORMULA = {
    0: "Normal Refresh (tRFC)",
    1: "FGR Mode (tRFC2)",
}
tCCD_L_FORMULA = {
    0: "8",
    1: "9",
    2: "10",
    3: "11",
    4: "12",
    5: "13",
    6: "14",
    7: "15",
    8: "16",
    9: "Reserved",
    10: "Reserved",
    11: "Reserved",
    12: "Reserved",
    13: "Reserved",
    14: "Reserved",
    15: "Reserved",
}
tCCD_L_WR_FORMULA = {
    0: "32",
    1: "36",
    2: "40",
    3: "44",
    4: "48",
    5: "52",
    6: "56",
    7: "60",
    8: "64",
    9: "Reserved",
    10: "Reserved",
    11: "Reserved",
    12: "Reserved",
    13: "Reserved",
    14: "Reserved",
    15: "Reserved",
}
DFE_TAP1_FORMULA = {
    0: "0", 1: "1", 2: "2", 3: "3", 4: "4", 5: "5", 6: "6", 7: "7", 
    8: "8", 9: "9", 10: "10", 11: "11", 12: "12", 13: "13", 14: "14", 15: "15", 
    16: "16", 17: "17", 18: "18", 19: "19", 20: "20", 21: "21", 22: "22", 23: "23", 
    24: "24", 25: "25", 26: "26", 27: "27", 28: "28", 29: "29", 30: "30", 31: "31", 
    32: "32", 33: "33", 34: "34", 35: "35", 36: "36", 37: "37", 38: "38", 39: "39", 
    40: "40", 41: "RFU", 42: "RFU", 43: "RFU", 44: "RFU", 45: "RFU", 46: "RFU", 
    47: "RFU", 48: "RFU", 49: "RFU", 50: "RFU", 51: "RFU", 52: "RFU", 53: "RFU", 
    54: "RFU", 55: "RFU", 56: "RFU", 57: "RFU", 58: "RFU", 59: "RFU", 60: "RFU", 
    61: "RFU", 62: "RFU", 63: "RFU", 64: "0", 65: "-1", 66: "-2", 67: "-3", 68: "-4", 
    69: "-5", 70: "-6", 71: "-7", 72: "-8", 73: "-9", 74: "-10", 75: "-11", 76: "-12", 
    77: "-13", 78: "-14", 79: "-15", 80: "-16", 81: "-17", 82: "-18", 83: "-19", 
    84: "-20", 85: "-21", 86: "-22", 87: "-23", 88: "-24", 89: "-25", 90: "-26", 
    91: "-27", 92: "-28", 93: "-29", 94: "-30", 95: "-31", 96: "-32", 97: "-33", 
    98: "-34", 99: "-35", 100: "-36", 101: "-37", 102: "-38", 103: "-39", 
    104: "-40", 105: "RFU", 106: "RFU", 107: "RFU", 108: "RFU", 109: "RFU", 
    110: "RFU", 111: "RFU", 112: "RFU", 113: "RFU", 114: "RFU", 115: "RFU", 
    116: "RFU", 117: "RFU", 118: "RFU", 119: "RFU", 120: "RFU", 121: "RFU", 
    122: "RFU", 123: "RFU", 124: "RFU", 125: "RFU", 126: "RFU", 127: "RFU"
}

DFE_TAP2_FORMULA = {
    0: "0", 1: "1", 2: "2", 3: "3", 4: "4", 5: "5", 6: "6", 7: "7",
    8: "8", 9: "9", 10: "10", 11: "11", 12: "12", 13: "13", 14: "14", 15: "15",
    16: "RFU", 17: "RFU", 18: "RFU", 19: "RFU", 20: "RFU", 21: "RFU", 22: "RFU", 23: "RFU",
    24: "RFU", 25: "RFU", 26: "RFU", 27: "RFU", 28: "RFU", 29: "RFU", 30: "RFU", 31: "RFU",
    32: "RFU", 33: "RFU", 34: "RFU", 35: "RFU", 36: "RFU", 37: "RFU", 38: "RFU", 39: "RFU",
    40: "RFU", 41: "RFU", 42: "RFU", 43: "RFU", 44: "RFU", 45: "RFU", 46: "RFU", 47: "RFU",
    48: "RFU", 49: "RFU", 50: "RFU", 51: "RFU", 52: "RFU", 53: "RFU", 54: "RFU", 55: "RFU",
    56: "RFU", 57: "RFU", 58: "RFU", 59: "RFU", 60: "RFU", 61: "RFU", 62: "RFU", 63: "RFU",
    64: "0", 65: "-1", 66: "-2", 67: "-3", 68: "-4", 69: "-5", 70: "-6", 71: "-7",
    72: "-8", 73: "-9", 74: "-10", 75: "-11", 76: "-12", 77: "-13", 78: "-14", 79: "-15",
    80: "RFU", 81: "RFU", 82: "RFU", 83: "RFU", 84: "RFU", 85: "RFU", 86: "RFU", 87: "RFU",
    88: "RFU", 89: "RFU", 90: "RFU", 91: "RFU", 92: "RFU", 93: "RFU", 94: "RFU", 95: "RFU",
    96: "RFU", 97: "RFU", 98: "RFU", 99: "RFU", 100: "RFU", 101: "RFU", 102: "RFU", 103: "RFU",
    104: "RFU", 105: "RFU", 106: "RFU", 107: "RFU", 108: "RFU", 109: "RFU", 110: "RFU", 111: "RFU",
    112: "RFU", 113: "RFU", 114: "RFU", 115: "RFU", 116: "RFU", 117: "RFU", 118: "RFU", 119: "RFU",
    120: "RFU", 121: "RFU", 122: "RFU", 123: "RFU", 124: "RFU", 125: "RFU", 126: "RFU", 127: "RFU",
}

DFE_TAP3_FORMULA = {
    0: "0", 1: "1", 2: "2", 3: "3", 4: "4", 5: "5", 6: "6", 7: "7", 8: "8", 9: "9",
    10: "10", 11: "11", 12: "12",
    13: "RFU", 14: "RFU", 15: "RFU",
    16: "RFU", 17: "RFU", 18: "RFU", 19: "RFU", 20: "RFU", 21: "RFU", 22: "RFU", 23: "RFU",
    24: "RFU", 25: "RFU", 26: "RFU", 27: "RFU", 28: "RFU", 29: "RFU", 30: "RFU", 31: "RFU",
    32: "RFU", 33: "RFU", 34: "RFU", 35: "RFU", 36: "RFU", 37: "RFU", 38: "RFU", 39: "RFU",
    40: "RFU", 41: "RFU", 42: "RFU", 43: "RFU", 44: "RFU", 45: "RFU", 46: "RFU", 47: "RFU",
    48: "RFU", 49: "RFU", 50: "RFU", 51: "RFU", 52: "RFU", 53: "RFU", 54: "RFU", 55: "RFU",
    56: "RFU", 57: "RFU", 58: "RFU", 59: "RFU", 60: "RFU", 61: "RFU", 62: "RFU", 63: "RFU",
    64: "0", 65: "-1", 66: "-2", 67: "-3", 68: "-4", 69: "-5", 70: "-6", 71: "-7",
    72: "-8", 73: "-9", 74: "-10", 75: "-11", 76: "-12",
    77: "RFU", 78: "RFU", 79: "RFU",
    80: "RFU", 81: "RFU", 82: "RFU", 83: "RFU", 84: "RFU", 85: "RFU", 86: "RFU", 87: "RFU",
    88: "RFU", 89: "RFU", 90: "RFU", 91: "RFU", 92: "RFU", 93: "RFU", 94: "RFU", 95: "RFU",
    96: "RFU", 97: "RFU", 98: "RFU", 99: "RFU", 100: "RFU", 101: "RFU", 102: "RFU", 103: "RFU",
    104: "RFU", 105: "RFU", 106: "RFU", 107: "RFU", 108: "RFU", 109: "RFU", 110: "RFU", 111: "RFU",
    112: "RFU", 113: "RFU", 114: "RFU", 115: "RFU", 116: "RFU", 117: "RFU", 118: "RFU", 119: "RFU",
    120: "RFU", 121: "RFU", 122: "RFU", 123: "RFU", 124: "RFU", 125: "RFU", 126: "RFU", 127: "RFU",
}

DFE_TAP4_FORMULA = {
    0: "0", 1: "1", 2: "2", 3: "3", 4: "4", 5: "5", 6: "6", 7: "7", 8: "8", 9: "9",
    10: "RFU", 11: "RFU", 12: "RFU", 13: "RFU", 14: "RFU", 15: "RFU",
    16: "RFU", 17: "RFU", 18: "RFU", 19: "RFU", 20: "RFU", 21: "RFU", 22: "RFU", 23: "RFU",
    24: "RFU", 25: "RFU", 26: "RFU", 27: "RFU", 28: "RFU", 29: "RFU", 30: "RFU", 31: "RFU",
    32: "RFU", 33: "RFU", 34: "RFU", 35: "RFU", 36: "RFU", 37: "RFU", 38: "RFU", 39: "RFU",
    40: "RFU", 41: "RFU", 42: "RFU", 43: "RFU", 44: "RFU", 45: "RFU", 46: "RFU", 47: "RFU",
    48: "RFU", 49: "RFU", 50: "RFU", 51: "RFU", 52: "RFU", 53: "RFU", 54: "RFU", 55: "RFU",
    56: "RFU", 57: "RFU", 58: "RFU", 59: "RFU", 60: "RFU", 61: "RFU", 62: "RFU", 63: "RFU",
    64: "0", 65: "-1", 66: "-2", 67: "-3", 68: "-4", 69: "-5", 70: "-6", 71: "-7", 72: "-8", 73: "-9",
    74: "RFU", 75: "RFU", 76: "RFU", 77: "RFU", 78: "RFU", 79: "RFU",
    80: "RFU", 81: "RFU", 82: "RFU", 83: "RFU", 84: "RFU", 85: "RFU", 86: "RFU", 87: "RFU",
    88: "RFU", 89: "RFU", 90: "RFU", 91: "RFU", 92: "RFU", 93: "RFU", 94: "RFU", 95: "RFU",
    96: "RFU", 97: "RFU", 98: "RFU", 99: "RFU", 100: "RFU", 101: "RFU", 102: "RFU", 103: "RFU",
    104: "RFU", 105: "RFU", 106: "RFU", 107: "RFU", 108: "RFU", 109: "RFU", 110: "RFU", 111: "RFU",
    112: "RFU", 113: "RFU", 114: "RFU", 115: "RFU", 116: "RFU", 117: "RFU", 118: "RFU", 119: "RFU",
    120: "RFU", 121: "RFU", 122: "RFU", 123: "RFU", 124: "RFU", 125: "RFU", 126: "RFU", 127: "RFU",
}

CA_ODT_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ/0.5 (480)",
    2: "RZQ/1 (240)",
    3: "RZQ/2 (120)",
    4: "RZQ/3 (80)",
    5: "RZQ/4 (60)",
    6: "RFU",
    7: "RZQ/6 (40)"
}
CA_ODT_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ/0.5 (480)",
    2: "RZQ/1 (240)",
    3: "RZQ/2 (120)",
    4: "RZQ/3 (80)",
    5: "RZQ/4 (60)",
    6: "RFU",
    7: "RZQ/6 (40)"
}
CS_ODT_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ/0.5 (480)",
    2: "RZQ/1 (240)",
    3: "RZQ/2 (120)",
    4: "RZQ/3 (80)",
    5: "RZQ/4 (60)",
    6: "RFU",
    7: "RZQ/6 (40)"
}
CK_ODT_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ/0.5 (480)",
    2: "RZQ/1 (240)",
    3: "RZQ/2 (120)",
    4: "RZQ/3 (80)",
    5: "RZQ/4 (60)",
    6: "RFU",
    7: "RZQ/6 (40)"
}
DQS_RTT_PARK_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ (240)",
    2: "RZQ/2 (120)",
    3: "RZQ/3 (80)",
    4: "RZQ/4 (60)",
    5: "RZQ/5 (48)",
    6: "RZQ/6 (40)",
    7: "RZQ/7 (34)"
}
RTT_PARK_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ (240)",
    2: "RZQ/2 (120)",
    3: "RZQ/3 (80)",
    4: "RZQ/4 (60)",
    5: "RZQ/5 (48)",
    6: "RZQ/6 (40)",
    7: "RZQ/7 (34)"
}
RTT_WR_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ (240)",
    2: "RZQ/2 (120)",
    3: "RZQ/3 (80)",
    4: "RZQ/4 (60)",
    5: "RZQ/5 (48)",
    6: "RZQ/6 (40)",
    7: "RZQ/7 (34)"
}
RTT_NOM_WR_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ (240)",
    2: "RZQ/2 (120)",
    3: "RZQ/3 (80)",
    4: "RZQ/4 (60)",
    5: "RZQ/5 (48)",
    6: "RZQ/6 (40)",
    7: "RZQ/7 (34)"
}
tWR_FORMULA = {
    0: 48,
    1: 54,
    2: 60,
    3: 66,
    4: 72,
    5: 78,
    6: 84,
    7: 90,
    8: 96
}
RTT_NOM_RD_FORMULA = {
    0: "RTT_OFF",
    1: "RZQ (240)",
    2: "RZQ/2 (120)",
    3: "RZQ/3 (80)",
    4: "RZQ/4 (60)",
    5: "RZQ/5 (48)",
    6: "RZQ/6 (40)",
    7: "RZQ/7 (34)"
}
RTT_Loopback_FORMULA = {
    0: "RTT_OFF",
    1: "RFU",
    2: "RFU",
    3: "RFU",
    4: "RFU",
    5: "RZQ/5 (48)",
    6: "RFU",
    7: "RFU"
}
# --- ODT latency offsets.
#
# Each field is a 3-bit code for a signed offset in memory clocks. Written as
# offsets and formatted by one function rather than as six literal tables:
# spelled out by hand they drifted, and ODTL WR OFF and ODTL WR NT OFF ended
# up printing "2 Clocks" where the other four printed "+2 Clocks" for the same
# kind of value. The reference tool signs all of them.
#
# A code a field does not define reads RFU, which is what the reference map
# calls the reserved ones -- not every field uses all eight.
def clock_offset(clocks):
    """A signed offset in clocks: "+2 Clocks", "-1 Clock", "0 Clocks"."""
    unit = "Clock" if abs(clocks) == 1 else "Clocks"
    return "0 %s" % unit if clocks == 0 else "%+d %s" % (clocks, unit)


def _odtl_table(offsets):
    return {
        code: clock_offset(offsets[code]) if code in offsets else "RFU"
        for code in range(8)
    }


# ON moves the termination earlier as the code rises, OFF later; the two RD NT
# fields reserve the ends of the range where the others do not.
ODTL_ON_WR = _odtl_table({1: -4, 2: -3, 3: -2, 4: -1, 5: 0, 6: 1, 7: 2})
ODTL_OFF_WR = _odtl_table({1: 4, 2: 3, 3: 2, 4: 1, 5: 0, 6: -1, 7: -2})
ODTL_ON_WR_NT = _odtl_table({1: -4, 2: -3, 3: -2, 4: -1, 5: 0, 6: 1, 7: 2})
ODTL_OFF_WR_NT = _odtl_table({1: 4, 2: 3, 3: 2, 4: 1, 5: 0, 6: -1, 7: -2})
ODTL_ON_RD_NT = _odtl_table({2: -3, 3: -2, 4: -1, 5: 0, 6: 1})
ODTL_OFF_RD_NT = _odtl_table({2: 3, 3: 2, 4: 1, 5: 0, 6: -1})

EN_DIS_FORMULA = {
    0: "Disabled",
    1: "Enabled"
}
VREF_FORMULA = {
    0: "97.5%", 1: "97.0%", 2: "96.5%", 3: "96.0%", 4: "95.5%", 5: "95.0%", 6: "94.5%", 7: "94.0%", 8: "93.5%", 9: "93.0%",
    10: "92.5%", 11: "92.0%", 12: "91.5%", 13: "91.0%", 14: "90.5%", 15: "90.0%", 16: "89.5%", 17: "89.0%", 18: "88.5%", 19: "88.0%",
    20: "87.5%", 21: "87.0%", 22: "86.5%", 23: "86.0%", 24: "85.5%", 25: "85.0%", 26: "84.5%", 27: "84.0%", 28: "83.5%", 29: "83.0%",
    30: "82.5%", 31: "82.0%", 32: "81.5%", 33: "81.0%", 34: "80.5%", 35: "80.0%", 36: "79.5%", 37: "79.0%", 38: "78.5%", 39: "78.0%",
    40: "77.5%", 41: "77.0%", 42: "76.5%", 43: "76.0%", 44: "75.5%", 45: "75.0%", 46: "74.5%", 47: "74.0%", 48: "73.5%", 49: "73.0%",
    50: "72.5%", 51: "72.0%", 52: "71.5%", 53: "71.0%", 54: "70.5%", 55: "70.0%", 56: "69.5%", 57: "69.0%", 58: "68.5%", 59: "68.0%",
    60: "67.5%", 61: "67.0%", 62: "66.5%", 63: "66.0%", 64: "65.5%", 65: "65.0%", 66: "64.5%", 67: "64.0%", 68: "63.5%", 69: "63.0%",
    70: "62.5%", 71: "62.0%", 72: "61.5%", 73: "61.0%", 74: "60.5%", 75: "60.0%", 76: "59.5%", 77: "59.0%", 78: "58.5%", 79: "58.0%",
    80: "57.5%", 81: "57.0%", 82: "56.5%", 83: "56.0%", 84: "55.5%", 85: "55.0%", 86: "54.5%", 87: "54.0%", 88: "53.5%", 89: "53.0%",
    90: "52.5%", 91: "52.0%", 92: "51.5%", 93: "51.0%", 94: "50.5%", 95: "50.0%", 96: "49.5%", 97: "49.0%", 98: "48.5%", 99: "48.0%",
    100: "47.5%", 101: "47.0%", 102: "46.5%", 103: "46.0%", 104: "45.5%", 105: "45.0%", 106: "44.5%", 107: "44.0%", 108: "43.5%", 109: "43.0%",
    110: "42.5%", 111: "42.0%", 112: "41.5%", 113: "41.0%", 114: "40.5%", 115: "40.0%", 116: "39.5%", 117: "39.0%", 118: "38.5%", 119: "38.0%",
    120: "37.5%", 121: "37.0%", 122: "36.5%", 123: "36.0%", 124: "35.5%", 125: "35.0%",
}
TIMINGS = [
    {"name": "CPU", "value": get_cpu_name(), "Category": "General", "Tab": "CPU", "Column": "Left", "read_type": "standard"},
    {"name": "Motherboard", "value": get_motherboard_name(), "Category": "General", "Tab": "CPU", "Column": "Left", "read_type": "standard"},
    {"name": "Capacity", "value": get_total_physical_memory(), "Category": "General", "Tab": "Timings", "Column": "Left", "read_type": "standard"},
    {"name": "Speed", "value": None, "Category": "General", "Tab": "Timings", "Column": "Left", "read_type": "standard"},
    {"name": "Dram Ratio", "address": MCHBAR + 0x5E04, "Category": "General", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "Multiplier", "address": MCHBAR + 0x5E04, "Category": "General", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 4}, "Column": "Left", "Formula": MULTIPLIER_FORMULA, "read_type": "standard"},
    {"name": "Gear Mode", "address": MCHBAR + 0x5E04, "Category": "General", "Tab": "Timings", "parameters": {"bit_start": 12, "bit_length": 2}, "Column": "Left", "Formula": GEAR_MODE_FORMULA, "read_type": "standard"},
    {"name": "CMD Stretch", "value": get_cmd_stretch(), "Category": "General", "Tab": "Timings", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "Channels", "value": detect_dual_channel_memory(), "Category": "General", "Tab": "Timings", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "IMC Frequency", "value": get_imc_freq(), "Category": "Frequency", "Tab": "CPU", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "BCLK", "value": get_bclk_rd(), "Category": "Frequency", "Tab": "CPU", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "RING", "value": get_ring_freq(), "Category": "Frequency", "Tab": "CPU", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "SA Voltage", "value": get_sa(), "Category": "Voltage", "Tab": "CPU", "parameters": {}, "Column": "Right", "read_type": "standard"},
    {"name": "TX Voltage", "value": get_tx(), "Category": "Voltage", "Tab": "CPU", "parameters": {}, "Column": "Right", "read_type": "standard"},
    {"name": "tCL", "address": MCHBAR + 0xE070, "Category": "Primary", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tRCD", "address": MCHBAR + 0xE004, "Category": "Primary", "Tab": "Timings", "parameters": {"bit_start": 19, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tRCDW", "address": MCHBAR + 0xE000, "Category": "Primary", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tRP", "address": MCHBAR + 0xE000, "Category": "Primary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tRAS", "address": MCHBAR + 0xE004, "Category": "Primary", "Tab": "Timings", "parameters": {"bit_start": 10, "bit_length": 9}, "Column": "Left", "read_type": "standard"},
    {"name": "tRC", "value": get_trc_value, "Category": "Primary", "Tab": "Timings", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "CR", "value": get_command_rate, "Category": "Primary", "Tab": "Timings", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "tRRD_S", "address": MCHBAR + 0xE008, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 9, "bit_length": 6}, "Column": "Left", "read_type": "standard"},
    {"name": "tRRD_L", "address": MCHBAR + 0xE008, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 15, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tWR", "Category": "Secondary", "Tab": "Timings", "Column": "Left", "read_type": "dynamic", "dynamic_params": {"offset_start": 0xE600, "value_to_find": 0x06, "offset_base": 0xE200, "bit_start_dynamic": 0, "bit_length_dynamic": 4, "mchbar": 0xFEDC0000, "command": 0, "offset2": 0,}, "Formula": tWR_FORMULA},
    {"name": "tRTP", "address": MCHBAR + 0xE000, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 13, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tFAW", "address": MCHBAR + 0xE008, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 9}, "Column": "Left", "read_type": "standard"},
    {
        "name": "tWTR_S",
        "value": get_tWTR_S,
        "Category": "Secondary",
        "Tab": "Timings",
        "parameters": {},
        "Column": "Left",
        "read_type": "standard"
    },
    {
        "name": "tWTR_L",
        "value": get_tWTR_L,
        "Category": "Secondary",
        "Tab": "Timings",
        "parameters": {},
        "Column": "Left",
        "read_type": "standard"
    },
    {"name": "tCWL", "address": MCHBAR + 0xE070, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    # DEC_TCWL, the trained write-leveling decrement applied to tCWL, which is
    # why it sits directly under it. See DEC_TCWL_OFFSET for where the
    # register came from and what about it is not yet confirmed.
    {"name": "DEC_TCWL", "address": MCHBAR + 0xE478, "Category": "Secondary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 4}, "Column": "Left", "read_type": "standard"},
    {"name": "tCCDL", "Category": "Secondary", "Tab": "Timings", "Column": "Left", "read_type": "dynamic", "dynamic_params": {"offset_start": 0xE600, "value_to_find": 0x00, "offset_base": 0xE200, "bit_start_dynamic": 0, "bit_length_dynamic": 4, "mchbar": 0xFEDC0000, "command": 0, "offset2": 0,}, "Formula": tCCD_L_FORMULA},
    {"name": "tCCDL WR", "Category": "Secondary", "Tab": "Timings", "Column": "Left", "read_type": "dynamic", "dynamic_params": {"offset_start": 0xE600, "value_to_find": 0x00, "offset_base": 0xE200, "bit_start_dynamic": 0, "bit_length_dynamic": 4, "mchbar": 0xFEDC0000, "command": 0, "offset2": 0,}, "Formula": tCCD_L_WR_FORMULA},
    {"name": "tRDRD_sg", "address": MCHBAR + 0xE00C, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDRD_dg", "address": MCHBAR + 0xE00C, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDRD_dr", "address": MCHBAR + 0xE00C, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDRD_dd", "address": MCHBAR + 0xE00C, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDWR_sg", "address": MCHBAR + 0xE010, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDWR_dg", "address": MCHBAR + 0xE010, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDWR_dr", "address": MCHBAR + 0xE010, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tRDWR_dd", "address": MCHBAR + 0xE010, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRRD_sg", "address": MCHBAR + 0xE014, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRRD_dg", "address": MCHBAR + 0xE014, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 9, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRRD_dr", "address": MCHBAR + 0xE014, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 18, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRRD_dd", "address": MCHBAR + 0xE014, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 25, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRWR_sg", "address": MCHBAR + 0xE018, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRWR_dg", "address": MCHBAR + 0xE018, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRWR_dr", "address": MCHBAR + 0xE018, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRWR_dd", "address": MCHBAR + 0xE018, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 7}, "Column": "Right", "read_type": "standard"},
    # Bit 7 of the register tRDRD_sg reads bits 0-6 of, so it sits with
    # the turnarounds rather than with a register of its own. Kept to the
    # foot of the section: it is a back-to-back allowance, not a delay,
    # and between tRDRD_sg and tRDRD_dg it split the group it belongs to.
    {"name": "Allow 2cyc B2B LPDDR", "address": MCHBAR + 0xE00C, "Category": "Tertiary", "Tab": "Timings", "parameters": {"bit_start": 7, "bit_length": 1}, "Column": "Right", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "tREFI", "address": MCHBAR + 0xE43C, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 18}, "Column": "Right", "read_type": "standard"},
    {"name": "tREFIx9", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "tRFC", "address": MCHBAR + 0xE43C, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 18, "bit_length": 13}, "Column": "Right", "read_type": "standard"},
    {"name": "tRFCpb", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 10, "bit_length": 11}, "Column": "Right", "read_type": "standard"},
    # The rest of 0xE488, which is the per-bank refresh register tRFCpb
    # already reads from: PBR is per-bank refresh, and its four controls
    # sit below tRFCpb's bits 10-20 with the ABR release above them.
    # All five match the reference tool's dump on this bench.
    {"name": "PBR Disable", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 1}, "Column": "Right", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "PBR OOO Disable", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 1, "bit_length": 1}, "Column": "Right", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "PBR Disable on hot", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 3, "bit_length": 1}, "Column": "Right", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "PBR Exit on idle", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 4, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    {"name": "Refresh ABR release", "address": MCHBAR + 0xE488, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 21, "bit_length": 4}, "Column": "Right", "read_type": "standard"},
    {"name": "tZQCAL", "address": MCHBAR + 0xE44C, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 13}, "Column": "Right", "read_type": "standard"},
    {"name": "tZQCS", "address": MCHBAR + 0xE448, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 10, "bit_length": 11}, "Column": "Right", "read_type": "standard"},
    {"name": "ZQCS period", "address": MCHBAR + 0xE448, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 10}, "Column": "Right", "read_type": "standard"},
    {"name": "tZQoper", "address": MCHBAR + 0xE440, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 13, "bit_length": 11}, "Column": "Right", "read_type": "standard"},
    {"name": "tREFSBRD", "address": MCHBAR + 0xE00A, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "tMOD", "address": MCHBAR + 0xE440, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "tCAL", "address": MCHBAR + 0xE08C, "Category": "Other Timings", "Tab": "Timings", "parameters": {"bit_start": 3, "bit_length": 3}, "Column": "Right", "read_type": "standard"},
    {"name": "tWRPDEN", "address": MCHBAR + 0xE054, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 10,}, "Column": "Left", "read_type": "standard"},
    {"name": "tRDPDEN", "address": MCHBAR + 0xE050, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 21, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tPRPDEN", "address": MCHBAR + 0xE054, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 27, "bit_length": 5}, "Column": "Left", "read_type": "standard"},
    {"name": "tAONPD", "address": MCHBAR + 0xE074, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 6}, "Column": "Left", "read_type": "standard"},
    {"name": "tCPDED", "address": MCHBAR + 0xE08C, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 24, "bit_length": 5}, "Column": "Left", "read_type": "standard"},
    {"name": "tCKE", "address": MCHBAR + 0xE050, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tXP", "address": MCHBAR + 0xE050, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 7, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tWRPRE", "address": MCHBAR + 0xE004, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 10}, "Column": "Left", "read_type": "standard"},
    # tWPRE is the write preamble, one bit: clear is 1 tCK and set is 2 tCK.
    #
    # 0xE478 tracked it across three BIOS settings, 1 -> 2 -> 1, identically in
    # all four sub-channel copies. That alone did not settle it, because this
    # file used to label bits 0-5 of the same register "DEC tCWL", and a 2 tCK
    # write preamble forces exactly a one-clock write-latency shift - so a
    # consequence fitted the evidence as well as the cause did.
    #
    # Changing tCWL 17 -> 15 and nothing else separated them: 0xE070 moved and
    # 0xE478 did not, on either controller. The register does not follow tCWL,
    # so it is not a write-latency field, and the old label was wrong.
    {"name": "tWPRE", "address": MCHBAR + 0xE478, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 1}, "Column": "Left", "read_type": "standard", "Formula": {0: "1", 1: "2"}},
    {"name": "tXPDLL", "address": MCHBAR + 0xE050, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 14, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tXSDLL", "address": MCHBAR + 0xE440, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 13}, "Column": "Left", "read_type": "standard"},
    # tXSR: located by setting it to 447 in BIOS and diffing full snapshots of
    # both controllers across the reboot. 0xE4C0 went 0x134 -> 0x1BF, i.e. 308
    # -> 447, on both controllers and in both sub-channel copies, with no other
    # bit in the register set either side. A control pair of snapshots taken
    # across a reboot that changed nothing the controller accepts shows this
    # register does not move on its own, unlike 0xE034/0xE040/0xE474/0xE4B0.
    {"name": "tXSR", "address": MCHBAR + 0xE4C0, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 13}, "Column": "Left", "read_type": "standard"},
    # tCKCKEH: located the same way, and the only field in the entire 32768
    # register sweep of both controllers to make the BIOS's own 20 -> 13
    # transition on both. The width is the open question rather than the
    # location: bits 12-14 read zero at both settings so the field could end
    # anywhere from bit 11, but bit 15 is set and unchanged either side, so 8
    # bits stops at the first boundary the data actually shows. Reading too
    # wide would surface a visibly wrong number if a neighbour ever fills;
    # reading too narrow would silently truncate a large value.
    {"name": "tCKCKEH", "address": MCHBAR + 0xE08C, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 7, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tPPD", "address": MCHBAR + 0xE000, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 20, "bit_length": 4}, "Column": "Left", "read_type": "standard"},
    {"name": "tCSH", "address": MCHBAR + 0xE054, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 10, "bit_length": 6}, "Column": "Left", "read_type": "standard"},
    {"name": "tCSL", "address": MCHBAR + 0xE054, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 6}, "Column": "Left", "read_type": "standard"},
    {"name": "tCA2CS", "address": MCHBAR + 0xE054, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 22, "bit_length": 5}, "Column": "Left", "read_type": "standard"},
    {"name": "OREF_RI", "address": MCHBAR + 0xE438, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    # The rest of 0xE438. OREF_RI holds bits 0-7 and tREFIx9 bits 24-31,
    # and the reference tool's six names tile the gap between them exactly:
    # 8-11, 12-15, 16, 17, 18-19, 20-23. Read on the bench, all six agree
    # with its dump -- 6, 7, 0, 1, 2, 5 -- which is what pins the layout,
    # since a register that only half matched would not.
    {"name": "Refresh HP WM", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 4}, "Column": "Left", "read_type": "standard"},
    {"name": "Refresh panic WM", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 12, "bit_length": 4}, "Column": "Left", "read_type": "standard"},
    {"name": "CounttREFIWhileRefEnOff", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 16, "bit_length": 1}, "Column": "Left", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "HPRefOnMRS", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 17, "bit_length": 1}, "Column": "Left", "read_type": "standard", "Formula": {0: "Disabled", 1: "Enabled"}},
    {"name": "SRX_Ref_Debits", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 18, "bit_length": 2}, "Column": "Left", "read_type": "standard"},
    {"name": "RAISE_BLK_WAIT", "address": MCHBAR + 0xE438, "Category": "Refresh timings", "Tab": "Timings", "parameters": {"bit_start": 20, "bit_length": 4}, "Column": "Left", "read_type": "standard"},
    # The reference tools' POWERDOWN group carries six more than we did.
    #
    # tSR is written there as bit 52 of a 64-bit read at 0xE4C0. That is the
    # upper dword, so it is bits 20-25 of 0xE4C4 read as a plain field, the
    # same way tXSR above takes the lower half of the same register.
    {"name": "tSR", "address": MCHBAR + 0xE4C4, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 20, "bit_length": 6}, "Column": "Left", "read_type": "standard"},
    {"name": "tOSCO", "address": MCHBAR + 0xE494, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "tPREMRR", "address": MCHBAR + 0xE494, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 8, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    # 0xE494 also holds tMRRMRW at bits 15-21, reading 90. Not carried: it
    # was added here once standing in for a tPREMRW that neither reference
    # tool defines, and nothing has asked for it since.
    # Directly below tMRR in the same register, and reads 90 against the
    # reference tool's 90.
    {"name": "tMRRMRW", "address": MCHBAR + 0xE494, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 15, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tMRR", "address": MCHBAR + 0xE494, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 22, "bit_length": 7}, "Column": "Left", "read_type": "standard"},
    {"name": "tRFM", "address": MCHBAR + 0xE40C, "Category": "Power down", "Tab": "Timings", "parameters": {"bit_start": 0, "bit_length": 11}, "Column": "Left", "read_type": "standard"},
    {"name": "tDLLK", "value": get_dllk_timing, "Category": "Power down", "Tab": "Timings", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {
    "name": "RTT WR",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x22,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 0,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x22,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 0,
        "offset2": 0,  
    },
    "Formula": RTT_WR_FORMULA
    },
    {
    "name": "RTT NOM RD",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x23,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 0,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x23,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 0,
        "offset2": 0,
    },
    "Formula": RTT_NOM_RD_FORMULA
    },
    {
    "name": "RTT NOM WR",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x23,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 0,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x23,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 0,
        "offset2": 0,  
    },
    "Formula": RTT_NOM_WR_FORMULA
    },
    {
    "name": "RTT PARK",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x08,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x08,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,  
    },
    "Formula": RTT_PARK_FORMULA
    },
    {
    "name": "RTT PARK DQS",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x06,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x06,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,  
    },
    "Formula": DQS_RTT_PARK_FORMULA
    },
    {
    "name": "RTT LOOPBACK",
    "Category": "RTT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x24,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 0,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x24,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 0,
        "offset2": 0,  
    },
    "Formula": RTT_Loopback_FORMULA
    },
    {
    "name": "CA ODT GROUP A",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,  
    },
    "Formula": CA_ODT_FORMULA
    },
    {
    "name": "CS ODT GROUP A",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x02,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,  
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x02,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,  
    },
    "Formula": CS_ODT_FORMULA
    },
    {
    "name": "CK ODT GROUP A",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x01,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,  
        "command": 1,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x01,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,  
    },
    "Formula": CK_ODT_FORMULA
    },
    {
    "name": "CA ODT GROUP B",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x07,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x07,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,
    },
    "Formula": CA_ODT_FORMULA
    },
    {
    "name": "CS ODT GROUP B",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x04,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,
        "command": 1,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x04,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0,
    },
    "Formula": CS_ODT_FORMULA
    },
    {
    "name": "CK ODT GROUP B",
    "Category": "ODT",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x03,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,  
        "command": 1,
        "offset2": 0,
    },
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x03,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,
        "command": 1,
        "offset2": 0, 
    },
    "Formula": CK_ODT_FORMULA
    },
    {
    "name": "PULL UP",
    "Category": "RON",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 1,
        "bit_length_dynamic": 2,
        "mchbar": 0xFEDC0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": RON_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 1,
        "bit_length_dynamic": 2,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": RON_FORMULA,
    },
    {
    "name": "PULL DN",
    "Category": "RON",
    "Tab": "Skew",
    "Column": "Left",
    "parameter_name": "Name",
    "name_a": "CHA",
    "name_b": "CHB",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 6,
        "bit_length_dynamic": 2,
        "mchbar": 0xFEDC0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": RON_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0x05,
        "offset_base": 0xE200,
        "bit_start_dynamic": 6,
        "bit_length_dynamic": 2,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": RON_FORMULA,
    },
    {"name": "WrDS Up", "address": MCHBAR + 0x2CE8, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDS Dn", "address": MCHBAR + 0x2CE8, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RdODT Up", "address": MCHBAR + 0x2CE8, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RdODT Dn", "address": MCHBAR + 0x2CE8, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSCmd Up", "address": MCHBAR + 0x2CEC, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSCtl Up", "address": MCHBAR + 0x2CEC, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSClk Up", "address": MCHBAR + 0x2CEC, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSCke CS Up", "address": MCHBAR + 0x2CEC, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    # CKE/CS has an up level and no down level, so there is deliberately no
    # WrDSCke CS Dn row. One was added here -- as CKE CS VREFDN, before these
    # rows took the reference tools' names -- on the assumption that every
    # level comes in a pair split across 0x2CEC and 0x2CF0 on matching bits,
    # and it read bits 24-31 of 0x2CF0 by symmetry with the up register.
    # Three things say that byte is not a CKE/CS level: the reference map
    # stops after WrDSClk Dn and defines nothing at bit 24 of this register,
    # the string ckecs_vrefdn appears in neither tool's binary, and BIOS
    # exposes a CKE/CS up setting with no down one beside it. The byte is
    # real and reads 80; what it is remains unknown, which is why it is no
    # longer given a name that implies otherwise.
    #
    # The receiver reference and the QX comparator count. Neither is one of
    # the up/down pairs above: RX VREF is a 9-bit level rather than an 8-bit
    # one, and QXCOUNT is a count rather than a voltage at all, kept with the
    # VREF rows because it is what the comparator those levels feed reports.
    # Read 374 and 30 respectively on the bench.
    {"name": "RX VREF", "address": MCHBAR + 0x008C, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 14, "bit_length": 9}, "Column": "Right", "read_type": "standard"},
    {"name": "QXCOUNT", "address": MCHBAR + 0x3C94, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 18, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSCmd Dn", "address": MCHBAR + 0x2CF0, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSCtl Dn", "address": MCHBAR + 0x2CF0, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "WrDSClk Dn", "address": MCHBAR + 0x2CF0, "Category": "VREF", "Tab": "Skew", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    # DQ VREF is per device, not per channel: the table entry points at a
    # base and the four DRAM devices sit in the four bytes from there, so the
    # device is chosen by walking the base one byte at a time. Reading only
    # the first reported one device's level as though it were the module's.
    #
    # Confirmed on the bench: the entry at 0xE770 points at 0xE220, whose four
    # bytes are 69/66/69/68 and decode to 63.0 / 64.5 / 63.0 / 63.5 percent.
    *[
        {
            "name": "DQ VREF D%d" % device,
            "Category": "VREF Additional",
            "Tab": "Skew",
            "Column": "Right",
            "read_type": "dynamic",
            "dynamic_params": {
                "offset_start": 0xE600,
                "value_to_find": 0x0A,
                "offset_base": 0xE200 + device,
                "bit_start_dynamic": 0,
                "bit_length_dynamic": 7,
                "mchbar": 0xFEDC0000,
                "command": 0,
                "offset2": 0,
            },
            "Formula": VREF_FORMULA,
        }
        for device in range(4)
    ],
    {
    "name": "CA VREF",
    "Category": "VREF Additional",
    "Tab": "Skew",
    "Column": "Right",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x0B,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 7,
        "mchbar": 0xFEDC0000,
        "command": 2,
        "offset2": 0,
    },
    "Formula": VREF_FORMULA,
    },
    {
    "name": "CS VREF",
    "Category": "VREF Additional",
    "Tab": "Skew",
    "Column": "Right",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x0C,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 7,
        "mchbar": 0xFEDC0000,
        "command": 2,
        "offset2": 0,
    },
    "Formula": VREF_FORMULA,
    },
    # 0x01BC tiles as CODEPI 0-5, CODEWL 6-11, BWSEL 12-17. CODEWL and
    # BWSEL match the reference tool exactly; CODEPI is a live DLL phase
    # code and was seen moving between 35 and 37 while sampled, so it is
    # read rather than compared against a captured number.
    {"name": "DLL_CODEPI", "address": MCHBAR + 0x01BC, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    {"name": "DLL_CODEWL", "address": MCHBAR + 0x01BC, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 6, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    {"name": "DLL BWSEL", "address": MCHBAR + 0x01BC, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 12, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    # The rest of the bandwidth-select and receive-enable group that sits with
    # DLL BWSEL. Verified on the bench at 4, 128 and 1253.
    {"name": "BWSEL LO Threshold", "address": MCHBAR + 0x3CA4, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 16, "bit_length": 6}, "Column": "Right", "read_type": "standard"},
    {"name": "DCC Control Code", "address": MCHBAR + 0x2C38, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 11, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RcvEn PI", "address": MCHBAR + 0x00F4, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 12}, "Column": "Right", "read_type": "standard"},
    #{"name": "ODTFINETUNE_CHA", "address": None, "Category": "MISC Additional", "Tab": "Skew", "parameters": {}, "Column": "Right", "read_type": "standard"},
    #{"name": "ODTFINETUNE_CHB", "address": None, "Category": "MISC Additional", "Tab": "Skew", "parameters": {}, "Column": "Right", "read_type": "standard"},
    {"name": "VTT ODT", "address": MCHBAR + 0x017C, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 0, "bit_length": 1},"Formula": EN_DIS_FORMULA, "Column": "Right", "read_type": "standard"},
    {"name": "VSS ODT", "address": MCHBAR + 0x017C, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 1, "bit_length": 1},"Formula": EN_DIS_FORMULA, "Column": "Right", "read_type": "standard"},
    {"name": "VDDQ ODT", "address": MCHBAR + 0x017C, "Category": "MISC Additional", "Tab": "Skew", "parameters": {"bit_start": 2, "bit_length": 1},"Formula": EN_DIS_FORMULA, "Column": "Right", "read_type": "standard"},
    
    # 0xE070 holds all four read/write duration and delay fields, one nibble
    # each. These rows used to carry only the two write fields and label the
    # two channel columns "RD" and "WR", which read as though the pair were
    # read and write -- they were the same bits on the two controllers, and
    # the actual read fields were not shown at all. The columns are the
    # channels here, as everywhere else on this tab.
    {"name": "ODT Read Duration", "address_a": MCHBAR + 0xE070, "address_b": CHANNEL_B + 0xE070, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 0, "bit_length": 4}, "parameters_b": {"bit_start": 0, "bit_length": 4}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {"name": "ODT Read Delay", "address_a": MCHBAR + 0xE070, "address_b": CHANNEL_B + 0xE070, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 4, "bit_length": 4}, "parameters_b": {"bit_start": 4, "bit_length": 4}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {"name": "ODT Write Duration", "address_a": MCHBAR + 0xE070, "address_b": CHANNEL_B + 0xE070, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 8, "bit_length": 4}, "parameters_b": {"bit_start": 8, "bit_length": 4}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {"name": "ODT Write Delay", "address_a": MCHBAR + 0xE070, "address_b": CHANNEL_B + 0xE070, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 12, "bit_length": 4}, "parameters_b": {"bit_start": 12, "bit_length": 4}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {"name": "ODT FINETUNE", "address_a": MCHBAR + 0xE0B4, "address_b": CHANNEL_B + 0xE0B4, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 0, "bit_length": 4}, "parameters_b": {"bit_start": 0, "bit_length": 4}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {"name": "ODT Write Early ODT", "address_a": MCHBAR + 0xE074, "address_b": CHANNEL_B + 0xE074, "Category": "ODT DELAY", "Tab": "Skew", "parameters_a": {"bit_start": 6, "bit_length": 1}, "parameters_b": {"bit_start": 6, "bit_length": 1}, "Column": "Right", "read_type_a": "standard", "read_type_b": "standard"},
    {
    "name": "REFRESH",
    "Category": "REFRESH MODE",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x04,
        "offset_base": 0xE200,
        "bit_start_dynamic": 4,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0,
        "offset2": 0,
    },
    "Formula": REFRESH_MODE_FORMULA,
    },
    {
    "name": "ODTL WR ON",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x25,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_ON_WR,
    },
    {
    "name": "ODTL WR OFF",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x25,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDC0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_OFF_WR,
    },
    {
    "name": "ODTL WR NT ON",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x26,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_ON_WR_NT,
    },
    {
    "name": "ODTL WR NT OFF",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x26,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_OFF_WR_NT,
    },
    {
    "name": "ODTL RD NT ON",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x27,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_ON_RD_NT,
    },
    {
    "name": "ODTL RD NT OFF",
    "Category": "ODTL",
    "Tab": "Skew",
    "Column": "Left",
    "read_type": "dynamic",  
    "dynamic_params": {
        "offset_start": 0xE600,
        "value_to_find": 0x27,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 3,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": ODTL_OFF_RD_NT,
    },
    {"name": "RTL MC0 CHA R0", "address": MCHBAR + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R1", "address": MCHBAR + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R2", "address": MCHBAR + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R3", "address": MCHBAR + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R4", "address": MCHBAR + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R5", "address": MCHBAR + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R6", "address": MCHBAR + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHA R7", "address": MCHBAR + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "", "value": "", "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R0", "address": MCHBAR2 + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R1", "address": MCHBAR2 + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R2", "address": MCHBAR2 + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R3", "address": MCHBAR2 + 0xE020, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R4", "address": MCHBAR2 + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R5", "address": MCHBAR2 + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R6", "address": MCHBAR2 + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC1 CHA R7", "address": MCHBAR2 + 0xE024, "Category": "Latency CHA", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Left", "read_type": "standard"},
    {"name": "RTL MC0 CHB R0", "address": MCHBAR + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R1", "address": MCHBAR + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R2", "address": MCHBAR + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R3", "address": MCHBAR + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R4", "address": MCHBAR + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R5", "address": MCHBAR + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R6", "address": MCHBAR + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC0 CHB R7", "address": MCHBAR + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "", "value": "", "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R0", "address": MCHBAR2 + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R1", "address": MCHBAR2 + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R2", "address": MCHBAR2 + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R3", "address": MCHBAR2 + 0xE820, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R4", "address": MCHBAR2 + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 0, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R5", "address": MCHBAR2 + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 8, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R6", "address": MCHBAR2 + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 16, "bit_length": 8}, "Column": "Right", "read_type": "standard"},
    {"name": "RTL MC1 CHB R7", "address": MCHBAR2 + 0xE824, "Category": "Latency CHB", "Tab": "RTL", "parameters": {"bit_start": 24, "bit_length": 8}, "Column": "Right", "read_type": "standard"},

    {
    "name": "Global DFE Gain",
    "Category": "DFE",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0,  
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    },
    {
    "name": "Global DFE Tap-1",
    "Category": "DFE",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 1,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0,  
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 1,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    },
    {
    "name": "Global DFE Tap-3",
    "Category": "DFE",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0,  
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 3,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    },
    {
    "name": "DFE GAIN Value",
    "Category": "DFE2",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 4,
        "mchbar": 0xFEDC0000,
        "command": 0,  
        "offset2": 1,
    },
    "Formula": DFE_GAIN_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 4,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 1,
    },
    "Formula": DFE_GAIN_FORMULA,
    },
    {
    "name": "Global DFE Tap-2",
    "Category": "DFE2",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 2,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 2,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    },
    {
    "name": "Global DFE Tap-4",
    "Category": "DFE2",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 4,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 2, 
    },
    "Formula": DFE_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 4,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 2,
    },
    "Formula": DFE_ENABLE_FORMULA,
    },


    {
    "name": "DFE Tap-1",
    "Category": "Tap 1",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    },
    {
    "name": "DFE Tap-1 Value",
    "Category": "Tap 1",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP1_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xF9,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP1_FORMULA,
    },


    {
    "name": "DFE Tap-3",
    "Category": "Tap 3",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFB,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFB,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    },
    {
    "name": "DFE Tap-3 Value",
    "Category": "Tap 3",
    "Tab": "Jedec",
    "Column": "Left",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFB,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP3_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFB,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP3_FORMULA,
    },


    {
    "name": "DFE Tap-2",
    "Category": "Tap 2",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFA,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFA,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    },
    {
    "name": "DFE Tap-2 Value",
    "Category": "Tap 2",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFA,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP2_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFA,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP2_FORMULA,
    },

    
    {
    "name": "DFE Tap-4",
    "Category": "Tap 4",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFC,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFC,
        "offset_base": 0xE200,
        "bit_start_dynamic": 7,
        "bit_length_dynamic": 1,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP_ENABLE_FORMULA,
    },
    {
    "name": "DFE Tap-4 Value",
    "Category": "Tap 4",
    "Tab": "Jedec",
    "Column": "Right",
    "parameter_name": "Channel",
    "name_a": "A",
    "name_b": "B",
    "read_type_a": "dynamic",
    "read_type_b": "dynamic",
    "dynamic_params_a": {
        "offset_start": 0xE600,
        "value_to_find": 0xFC,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDC0000,
        "command": 0, 
        "offset2": 0, 
    },
    "Formula": DFE_TAP4_FORMULA,
    "dynamic_params_b": {
        "offset_start": 0xE600,
        "value_to_find": 0xFC,
        "offset_base": 0xE200,
        "bit_start_dynamic": 0,
        "bit_length_dynamic": 6,
        "mchbar": 0xFEDD0000,  
        "command": 0,
        "offset2": 0,
    },
    "Formula": DFE_TAP4_FORMULA,
    },
]
def _decode_ddr4_dq_vref():
    """Read the DDR4 MR6 VREFDQ shadow and return its programmed percentage.

    Raptor Lake DDR4 exposes MR6 in the channel MR shadow at MCHBAR + 0xE5AC.
    The trained value is bits [5:0] and the range selector is bit 7.

    Bit 12 was read as the range selector for a while, which put this row at
    63.20% instead of 78.20%. Bit 12 is the top of tCCD_L: bits [12:10] read
    4 here, and 4 decodes to tCCD_L 8, which is exactly what the BIOS shows.
    A range bit at 12 would have left tCCD_L at [11:10] = 0, meaning 4, and
    the BIOS says 8 -- so bit 12 belongs to tCCD_L and the range selector is
    the JEDEC one. Both bit 6 and bit 7 read 0 here, so the value is the same
    whichever of the two conventions MR6 is written under.
    """
    for channel_base in (MCHBAR, MCHBAR2):
        try:
            raw = read_physical_memory_int(channel_base + 0xE5AC)
            if raw is None or raw == 0:
                continue

            value_code = int(raw) & 0x3F
            range_two = (int(raw) >> 7) & 0x1
            if value_code > 50:
                continue

            base_percent = 45.0 if range_two else 60.0
            percent = base_percent + (value_code * 0.65)
            if 45.0 <= percent <= 92.5:
                return f"{percent:.2f}%"
        except Exception as e:
            print(f"Error reading DDR4 DQ VREF: {e}")
    return "N/A"


# The DDR5 rows are per DRAM device; DDR4 trains one VREFDQ per rank, so on
# that generation the four collapse into the one row below.
DDR4_DQ_VREF_PREFIX = "DQ VREF D"
DDR4_DQ_VREF_FIRST = "DQ VREF D0"
DDR4_DQ_VREF_NAME = "DQ VREF"
DDR4_DROP_MARKER = "_drop_on_ddr4"

# VREF rows DDR4 has no counterpart for. DDR5 trains VREFCA and VREFCS in
# MR11 and MR12; DDR4 defines neither register, references the command bus
# against an external half-VDD supply and lets CS share it. Nothing on the
# module holds a value for either, so the rows come off rather than standing
# there restating that fact.
DDR4_ABSENT_VREF_ROWS = ("CA VREF", "CS VREF")


def _install_platform_vref_additional():
    """Use DDR-generation-correct VREF rows without CHA/CHB columns."""
    global TIMINGS
    generation = detect_ddr_generation()

    for timing in TIMINGS:
        if timing.get("Category") != "VREF Additional":
            continue

        # Remove the v21 dual-channel wrapper. Summary and Skew should show one
        # clean value column for DQ/CA/CS VREF.
        for key in (
            "read_type_a", "read_type_b", "dynamic_params_a", "dynamic_params_b",
            "parameters_a", "parameters_b", "value_a", "value_b", "name_a",
            "name_b", "parameter_name"
        ):
            timing.pop(key, None)

        name = timing.get("name")
        if generation == "DDR4":
            timing.pop("dynamic_params", None)
            timing.pop("address", None)
            timing.pop("parameters", None)
            timing.pop("Formula", None)
            timing["read_type"] = "standard"

            # DDR4 trains one VREFDQ per rank, not one per DRAM device, so the
            # four per-device rows collapse to a single reading. The first is
            # renamed and kept; the rest are marked for removal below.
            #
            # This branch matched a row called "DQ VREF" until the DDR5 work
            # split that row into D0-D3. Nothing then matched, so all four were
            # stripped of their source above and left permanently blank, while
            # the MR6 decoder that answers this on DDR4 went unused.
            if name.startswith(DDR4_DQ_VREF_PREFIX):
                if name == DDR4_DQ_VREF_FIRST:
                    timing["name"] = DDR4_DQ_VREF_NAME
                    timing["value"] = _decode_ddr4_dq_vref
                else:
                    timing["value"] = None
                    timing[DDR4_DROP_MARKER] = True
            elif name in DDR4_ABSENT_VREF_ROWS:
                # DDR4 has no VREFCA and no VREFCS. The command/address
                # reference is an external one at half VDD and CS rides on it,
                # so neither is a mode-register field and neither is trained
                # -- there is nothing on the module to read. These rows used
                # to print "50.0% (fixed)" and "Uses CA VREF", which are not
                # readings but the absence of one, written as though it were.
                timing["value"] = None
                timing[DDR4_DROP_MARKER] = True
            continue

        # DDR5 keeps the original MR10/MR11/MR12 dynamic readers.
        original = {
            "DQ VREF": {
                "offset_start": 0xE600, "value_to_find": 0x0A,
                "offset_base": 0xE200, "bit_start_dynamic": 0,
                "bit_length_dynamic": 7, "mchbar": 0xFEDC0000,
                "command": 0, "offset2": 0,
            },
            "CA VREF": {
                "offset_start": 0xE600, "value_to_find": 0x0B,
                "offset_base": 0xE200, "bit_start_dynamic": 0,
                "bit_length_dynamic": 7, "mchbar": 0xFEDC0000,
                "command": 2, "offset2": 0,
            },
            "CS VREF": {
                "offset_start": 0xE600, "value_to_find": 0x0C,
                "offset_base": 0xE200, "bit_start_dynamic": 0,
                "bit_length_dynamic": 7, "mchbar": 0xFEDC0000,
                "command": 2, "offset2": 0,
            },
        }
        if name in original:
            timing.pop("value", None)
            timing["read_type"] = "dynamic"
            timing["dynamic_params"] = original[name]
            timing["Formula"] = VREF_FORMULA

    TIMINGS = [
        timing for timing in TIMINGS if not timing.pop(DDR4_DROP_MARKER, False)
    ]


_install_platform_vref_additional()


def apply_formula(value, formula):
    if value is None:
        return "N/A"
    try:
        if isinstance(formula, dict):
            return formula.get(int(value), "N/A")
        elif callable(formula):
            return formula(value)
        else:
            return str(value)
    except (ValueError, TypeError) as e:
        print(f"Error applying formula: {e}")
        return "N/A"

def get_speed():
    try:
        if is_arrow_lake_platform():
            ratio = read_timing(
                MCHBAR + 0x13D10,
                bit_start=0,
                bit_length=8,
                read_type="standard",
            )
            if ratio is None:
                return "Unknown"
            # Core Ultra 200S MemSS PMA reports a 33.334 MHz QCLK reference.
            effective_rate = float(ratio) * 33.334 * 2.0
            return f"{round(effective_rate, 0)} Mhz"

        bclk = get_bclk()
        if not isinstance(bclk, (int, float)):
            return "Unknown"

        ratio = read_timing(MCHBAR + 0x5E04, bit_start=0, bit_length=8, read_type="standard")
        raw_multiplier = read_timing(MCHBAR + 0x5E04, bit_start=8, bit_length=4, read_type="standard")
        raw_gear = read_timing(MCHBAR + 0x5E04, bit_start=12, bit_length=2, read_type="standard")

        if ratio is None or raw_multiplier is None or raw_gear is None:
            return "Unknown"

        if raw_multiplier == 0:
            qclk_ratio = 133.333333
        elif raw_multiplier == 1:
            qclk_ratio = 100.0
        else:
            qclk_ratio = 100.0

        gear_divider = {0: 1, 1: 2, 2: 4}.get(raw_gear, 1)
        speed = float(ratio) * (qclk_ratio / 100.0) * float(bclk) * float(gear_divider)
        return f"{round(speed, 0)} Mhz"
    except Exception as e:
        print(f"Error calculating speed: {e}")
        return "Unknown"

for timing in TIMINGS:
    if timing["name"] == "Speed":
        timing["value"] = get_speed()
        break

# --- UI customization: rename Timings tab to Main, remove CPU tab, and surface CPU/general info on Main.
for timing in TIMINGS:
    if timing.get("Tab") == "Timings":
        timing["Tab"] = "Main"

# Normalize and fix specific General / Secondary rows.
for timing in TIMINGS:
    if timing.get("name") == "CPU" and timing.get("Tab") == "CPU":
        timing["Tab"] = "Main"
        timing["Category"] = "General"
    elif timing.get("name") == "Motherboard" and timing.get("Tab") == "CPU":
        timing["Tab"] = "Main"
        timing["Category"] = "General"
    elif timing.get("name") == "Capacity" and timing.get("Tab") == "Main" and timing.get("Category") == "General":
        timing["name"] = "Memory Capacity"
    elif timing.get("name") == "Speed" and timing.get("Tab") == "Main" and timing.get("Category") == "General":
        timing["name"] = "DRAM Frequency"
        timing["value"] = get_dram_frequency()
        timing["address"] = None
        timing["parameters"] = {}
        timing["read_type"] = "standard"
    elif timing.get("name") == "Dram Ratio" and timing.get("Tab") == "Main" and timing.get("Category") == "General":
        timing["name"] = "DRAM Ratio"
    elif timing.get("name") == "Multiplier" and timing.get("Tab") == "Main" and timing.get("Category") == "General":
        timing["name"] = "DDR QCLK Ratio"
        timing["value"] = get_qclk_ratio()
        timing["address"] = None
        timing["parameters"] = {}
        timing["read_type"] = "standard"
        timing.pop("Formula", None)
    elif timing.get("name") == "tWR" and timing.get("Tab") == "Main" and timing.get("Category") == "Secondary":
        timing["value"] = get_twr_value()
        timing["read_type"] = "standard"
        timing.pop("dynamic_params", None)

# Remove old CPU-tab rows that we are replacing with a custom ordered General section.
def _is_old_main_general_row(t):
    return t.get("Tab") == "Main" and t.get("Category") == "General"

TIMINGS = [t for t in TIMINGS if not _is_old_main_general_row(t)]

def get_power_down_mode_value():
    """Report the BIOS-controlled memory power-down policy.

    TC_PWRDN/tPPD contains exit and command timing values; it is not an
    enable flag.  Use DDR_PTM_CTL[6] on the tested Intel desktop paths instead.
    When that bit is set, BIOS owns the CKE power-down policy.  The calibrated
    Z690/Z790 DDR4 and Z890 DDR5 BIOS configurations used by Roch Viewer have
    Power Down Mode disabled under BIOS control.  When it is clear, P-code is
    allowed to manage the policy dynamically, which is reported as Automatic.
    """
    try:
        raw = _read_ddr_ptm_control()
        if raw is None:
            return "N/A"

        bios_controls_power_down = bool(int(raw) & (1 << 6))
        return "Disabled" if bios_controls_power_down else "Automatic"
    except Exception as e:
        print(f"Error reading power-down mode: {e}")
        return "N/A"


# --- Gear mode has a second witness.
#
# get_gear_mode_value() above reads MCHBAR + 0x5E04 bits 12-13. The scheduler
# register SC_GS_CFG at 0xE088 carries the same fact as two separate flags,
# and those two bits were placed by measurement on the LGA1700 DDR5 bench --
# a Gear 2 -> Gear 4 -> Gear 2 round trip, snapshotting the whole of MCHBAR
# at each step:
#
#     Gear 2   0x8FC00029     bit 15 = 0   bit 31 = 1
#     Gear 4   0x0FC08029     bit 15 = 1   bit 31 = 0
#     Gear 2   0x8FC00029     bit 15 = 0   bit 31 = 1
#
# Both transitions moved exactly those two bits and nothing else in the
# register, and it came back to its original value while 928 registers
# elsewhere did not survive the round trip unchanged. That is what places
# them rather than fits them. Worth recording that the obvious candidate
# beforehand was wrong: bit 0 is the only set bit below the known fields and
# it did not move.
#
# This is a check on the gear row, not a second way to show it. A duplicate
# row would print the same wrong answer twice if the field at 0x5E04 ever
# moved; two witnesses disagree instead, and the test says so.
SCHEDULER_CONFIG_OFFSET = 0xE088
SCHEDULER_GEAR4_BIT = 15
SCHEDULER_GEAR2_BIT = 31


def scheduler_gear_mode(base=None):
    """The gear as SC_GS_CFG reports it, or None when it cannot say.

    None rather than a guess when neither flag is set or both are: the whole
    value of a second witness is that it stays silent instead of agreeing by
    construction.
    """
    raw = read_physical_memory_int(
        (MCHBAR if base is None else base) + SCHEDULER_CONFIG_OFFSET, 4)
    if raw is None or int(raw) == 0xFFFFFFFF:
        return None
    raw = int(raw)
    gear2 = raw >> SCHEDULER_GEAR2_BIT & 1
    gear4 = raw >> SCHEDULER_GEAR4_BIT & 1
    if gear2 == gear4:
        return None
    return 4 if gear4 else 2


def _read_ddr_ptm_control():
    """Read Intel DDR power/thermal-management control when exposed."""
    try:
        raw = read_physical_memory_int(MCHBAR + 0x5880, 4)
        if raw is None:
            return None
        raw = int(raw)
        if raw == 0xFFFFFFFF:
            return None
        return raw
    except Exception as e:
        print(f"Error reading DDR_PTM_CTL: {e}")
        return None


# Refresh policy, DDR_PTM_CTL[3:2].
REFRESH_MODE_NORMAL = 0

# Labels are kept short on purpose. The row sits in a section whose other rows
# are per-channel, so its text lands in the narrow A1 column, and
# _align_dual_columns widens that column to its longest entry across every
# section on the tab. "Normal" costs the same width as the "144 ns" already
# there; the wordier original cost three times that. The 2x labels are longer,
# but they only appear when refresh is not on its default schedule, which is
# exactly when the row should draw the eye.
REFRESH_MODE_LABELS = {
    REFRESH_MODE_NORMAL: "Normal",
    1: "2x (Warm/Hot)",
    2: "2x (Hot)",
    3: "Invalid",
}


def _ddr4_refresh_mode_code():
    """Return the DDR_PTM_CTL[3:2] refresh policy code, or None."""
    if detect_ddr_generation() != "DDR4":
        return None
    raw = _read_ddr_ptm_control()
    if raw is None:
        return None
    return (raw >> 2) & 0x3


def get_ddr4_refresh_mode_value():
    """Decode the live DDR4 refresh policy."""
    code = _ddr4_refresh_mode_code()
    if code is None:
        return "N/A"
    return REFRESH_MODE_LABELS.get(code, "N/A")


def refresh_is_normal():
    """True when refresh is running its normal all-bank tRFC schedule.

    Read from the same register the Refresh Mode row displays, so a dimmed row
    and the mode above it can never disagree. Unknown counts as not normal:
    dimming a row the hardware might well be using would be the worse mistake.
    """
    return _ddr4_refresh_mode_code() == REFRESH_MODE_NORMAL


def get_ecc_error_correction_value():
    """Report system-memory error correction from SMBIOS, with DIMM-width fallback."""
    try:
        correction_names = {
            3: "Disabled",
            4: "Parity",
            5: "Single-bit ECC",
            6: "Multi-bit ECC",
            7: "CRC",
        }
        for array in _wmi_static("Win32_PhysicalMemoryArray"):
            try:
                code = int(getattr(array, "MemoryErrorCorrection", 0) or 0)
            except (TypeError, ValueError):
                continue
            if code in correction_names:
                return correction_names[code]

        saw_width = False
        for memory in _wmi_static("Win32_PhysicalMemory"):
            try:
                total_width = int(getattr(memory, "TotalWidth", 0) or 0)
                data_width = int(getattr(memory, "DataWidth", 0) or 0)
            except (TypeError, ValueError):
                continue
            if total_width > 0 and data_width > 0:
                saw_width = True
                if total_width > data_width:
                    return "Enabled"
        if saw_width:
            return "Disabled"
    except Exception as e:
        print(f"Error detecting memory error correction: {e}")
    return "N/A"


def get_self_refresh_value():
    """Report whether a valid self-refresh timing block is programmed."""
    try:
        # Raptor/Alder Lake TC_SRFTP is at MCHBAR + E440. A valid non-zero
        # timing block means the controller has self-refresh operation configured.
        if is_arrow_lake_platform():
            return "N/A"
        raw = read_physical_memory_int(MCHBAR + 0xE440, 4)
        if raw is None:
            return "N/A"
        raw = int(raw)
        if raw == 0xFFFFFFFF:
            return "N/A"
        return "Enabled" if raw != 0 else "Disabled"
    except Exception as e:
        print(f"Error reading self-refresh state: {e}")
        return "N/A"


def get_memory_scrambler_value():
    """Do not claim a scrambler state without a verified live status source."""
    return "Not Exposed"


def get_row_hammer_value():
    """Do not label DDR refresh policy as a verified row-hammer state."""
    return "Not Exposed"


# None of these are read on a timer. "live" is what puts a row on the refresh
# worker, and it belongs to the sensor rows alone -- those are readings that
# move continuously and have a window of their own that keeps their minimum,
# maximum and average. Everything on the reading tabs is a setting, shown as
# it was when the tab was drawn.
#
# The clocks and ratios hold their getter rather than the value it returned at
# import. That is not liveness: nothing calls them again on this tab. It means
# the Advanced window, which polls its own list, gets the current reading
# instead of a copy of startup.
#
# The identity rows above them hold a plain reading, because a CPU name,
# board, BIOS, microcode, module vendor and installed capacity cannot change
# while the machine runs and each one costs a WMI query.
custom_general_rows = [
    {"name": "CPU", "value": get_cpu_name(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Cores / Threads", "value": get_cpu_cores_threads(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Manufacturer", "value": get_board_manufacturer(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Model", "value": get_board_model(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "BIOS", "value": get_bios_version(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Microcode", "value": get_microcode(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "RAM Manufacturer", "value": lambda: _dimm_field("module_manufacturer") or get_ram_manufacturer(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Memory Capacity", "value": get_total_physical_memory(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "DRAM Frequency", "value": get_dram_frequency, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "CMD Stretch", "value": get_cmd_stretch(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "DRAM Ratio", "value": get_dram_ratio_value, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Gear Mode", "value": get_gear_mode_value, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Channels", "value": detect_dual_channel_memory(), "Category": "General", "Tab": "Main", "parameters": {}, "Column": "Left", "read_type": "standard"},
    {"name": "DDR QCLK Ratio", "value": get_qclk_ratio, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "BCLK", "value": get_bclk_rd, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Uncore", "value": get_ring_freq, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "MCLK", "value": get_mclk, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "UCLK", "value": get_uclk, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Power Down Mode", "value": get_power_down_mode_value(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "ECC / Error Correction", "value": get_ecc_error_correction_value(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Self Refresh", "value": get_self_refresh_value(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Memory Scrambler", "value": get_memory_scrambler_value(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "Row Hammer", "value": get_row_hammer_value(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "PSF0 PLL", "value": get_psf0_pll, "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "VDDQ TX", "value": get_tx(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
    {"name": "VCCSA", "value": get_sa(), "Category": "General", "Tab": "Main", "Column": "Left", "read_type": "standard"},
]

# Put the custom General section at the very front of Main so the order is exactly controlled.
TIMINGS = custom_general_rows + TIMINGS


# --- Core Ultra 200S / Z890 live timing map.
def _install_arrow_lake_main_timings():
    """Switch Main-tab primary/secondary fields to the Core Ultra 200S register layout."""
    if not is_arrow_lake_platform():
        return

    field_map = {
        # name: (offset, bit_start, bit_length)
        "tRCD": (0xE138, 22, 8),
        "tRCDW": (0xE13C, 0, 8),
        "tRAS": (0xE004, 13, 9),
        "tRRD_L": (0xE138, 9, 6),
        "tRRD_S": (0xE138, 15, 7),
        "tFAW": (0xE138, 0, 9),
        "tRFC": (0xE4A0, 18, 13),
        "tRFCpb": (0xE4A4, 8, 11),
        "tREFI": (0xE4A0, 0, 18),
        "tRTP": (0xE000, 20, 7),
        "tWRPRE": (0xE004, 1, 10),
        "tREFIx9": (0xE4A4, 0, 8),
    }

    for timing in TIMINGS:
        name = timing.get("name")
        if name in field_map:
            offset, bit_start, bit_length = field_map[name]
            timing["address"] = MCHBAR + offset
            timing["parameters"] = {
                "bit_start": bit_start,
                "bit_length": bit_length,
            }
            timing["read_type"] = "standard"
            timing.pop("value", None)
            timing.pop("default_value", None)
            timing.pop("dynamic_params", None)
            timing.pop("Formula", None)
        elif name == "tWR" and timing.get("Category") == "Secondary":
            timing["value"] = get_twr_value()
            timing["read_type"] = "standard"
            timing.pop("address", None)
            timing.pop("parameters", None)
            timing.pop("dynamic_params", None)
            timing.pop("Formula", None)
        elif name == "tMOD":
            timing["value"] = get_tmod_value()
            timing["read_type"] = "standard"
            timing.pop("address", None)
            timing.pop("parameters", None)
            timing.pop("dynamic_params", None)
            timing.pop("Formula", None)
        # tCCD_L and tCCD_L_WR are not handled here any more. _install_ccd_
        # timings replaces those rows on every Intel platform, from the same
        # register fields this branch used, so doing it again here would only
        # spend two eager reads on a value that is then thrown away.


_install_arrow_lake_main_timings()


def _install_trdpre_alias():
    """Expose Intel's tRDPRE name using the active platform tRTP/TC_PRE field."""
    if any(item.get("name") == "tRDPRE" for item in TIMINGS):
        return
    source = next((item for item in TIMINGS if item.get("name") == "tRTP"), None)
    if source is None:
        return
    alias = dict(source)
    alias["name"] = "tRDPRE"
    alias["Category"] = "Power down"
    alias["Tab"] = "Timings"
    alias["Column"] = "Left"
    if "parameters" in source:
        alias["parameters"] = dict(source["parameters"])
    if "dynamic_params" in source:
        alias["dynamic_params"] = dict(source["dynamic_params"])
    TIMINGS.append(alias)


_install_trdpre_alias()


def _reorder_power_down_timings():
    """Put the requested power-down timings first and retain the rest's order."""
    global TIMINGS
    leading_order = [
        "tCKE", "tXP", "tWRPRE", "tWRPDEN", "tRDPRE", "tRDPDEN",
    ]
    power_down_rows = [
        timing for timing in TIMINGS
        if timing.get("Tab") == "Timings"
        and timing.get("Category") == "Power down"
    ]
    if not power_down_rows:
        return

    leading_index = {name: index for index, name in enumerate(leading_order)}
    ordered_rows = sorted(
        enumerate(power_down_rows),
        key=lambda item: (
            0 if item[1].get("name") in leading_index else 1,
            leading_index.get(item[1].get("name"), len(leading_order)),
            item[0],
        ),
    )
    ordered_rows = [timing for _, timing in ordered_rows]

    result = []
    inserted = False
    for timing in TIMINGS:
        is_power_down = (
            timing.get("Tab") == "Timings"
            and timing.get("Category") == "Power down"
        )
        if is_power_down:
            if not inserted:
                result.extend(ordered_rows)
                inserted = True
            continue
        result.append(timing)
    TIMINGS = result


_reorder_power_down_timings()

# --- DDR4 live RTT / RON reader (verified on Alder/Raptor Lake DDR4 register shadows).
# DDR5 keeps the project's original Skew-tab readers until its register mapping is verified.
DDR4_RTT_NOM_PARK_FORMULA = {
    0b000: "Disabled",
    0b001: "60 Ohm",
    0b010: "120 Ohm",
    0b011: "40 Ohm",
    0b100: "240 Ohm",
    0b101: "48 Ohm",
    0b110: "80 Ohm",
    0b111: "34 Ohm",
}

DDR4_RTT_WR_LIVE_FORMULA = {
    0b000: "Disabled",
    0b001: "120 Ohm",
    0b010: "240 Ohm",
    0b011: "High-Z",
    0b100: "80 Ohm",
    0b101: "Reserved (101b)",
    0b110: "Reserved (110b)",
    0b111: "Reserved (111b)",
}

DDR4_RON_LIVE_FORMULA = {
    0b00: "34 Ohm",
    0b01: "48 Ohm",
    0b10: "Reserved (10b)",
    0b11: "Reserved (11b)",
}


def _read_live_field(base, offset, bit_start, bit_length):
    try:
        return read_timing(
            address=base + offset,
            bit_start=bit_start,
            bit_length=bit_length,
            read_type="standard",
        )
    except Exception as e:
        print(f"Error reading live field at {base + offset:#x}: {e}")
        return None


def _decode_live_rank_pair(base, offsets, bit_start, bit_length, formula):
    """Read both controller shadows. Show one value if they match, otherwise show both."""
    decoded = []
    for offset in offsets:
        raw = _read_live_field(base, offset, bit_start, bit_length)
        if raw is None:
            decoded.append("N/A")
        else:
            decoded.append(formula.get(int(raw), f"Unknown ({int(raw):#x})"))

    if decoded[0] == decoded[1]:
        return decoded[0]
    if decoded[0] == "N/A":
        return decoded[1]
    if decoded[1] == "N/A":
        return decoded[0]
    return f"R0 {decoded[0]} / R1 {decoded[1]}"


def get_ddr4_rtt_nom(base):
    return _decode_live_rank_pair(
        base, (0xE5A0, 0xF5A0), 24, 3, DDR4_RTT_NOM_PARK_FORMULA
    )


def get_ddr4_rtt_wr(base):
    return _decode_live_rank_pair(
        base, (0xE5A4, 0xF5A4), 9, 3, DDR4_RTT_WR_LIVE_FORMULA
    )


def get_ddr4_rtt_park(base):
    return _decode_live_rank_pair(
        base, (0xE5A8, 0xF5A8), 22, 3, DDR4_RTT_NOM_PARK_FORMULA
    )


def get_ddr4_ron(base):
    # DDR4 MR1 A2:A1 selects the nominal output-driver impedance. The selected
    # driver applies to both pull-up and pull-down, so both UI rows are read from
    # the same live field instead of using motherboard-specific preset values.
    return _decode_live_rank_pair(
        base, (0xE5A0, 0xF5A0), 17, 2, DDR4_RON_LIVE_FORMULA
    )


def _make_dual_live_row(name, category, read, base_a=MCHBAR,
                        base_b=CHANNEL_B):
    """A per-channel Skew row that reads both controllers on every refresh.

    Takes the reader rather than two readings. It used to take the values,
    which meant each row froze at whatever the DRAM had been told at startup
    -- fine for settings that never move, wrong the moment one does, and
    indistinguishable on screen from a live reading that happens to be steady.
    """
    # Empty parameter dictionaries intentionally mark this as a dual-column row;
    # main.py resolves the value_a/value_b getters.
    return {
        "name": name,
        "Category": category,
        "Tab": "Skew",
        "Column": "Left",
        "parameter_name": "Name",
        "name_a": "CHA",
        "name_b": "CHB",
        "parameters_a": {},
        "parameters_b": {},
        "value_a": lambda: read(base_a),
        "value_b": lambda: read(base_b),
        "read_type_a": "standard",
        "read_type_b": "standard",
    }


def _install_ddr4_skew_live_rows():
    global TIMINGS
    if detect_ddr_generation() != "DDR4":
        return

    # Replace only RTT and RON. ODT and ODT timing are deliberately left alone.
    TIMINGS = [
        timing for timing in TIMINGS
        if not (
            timing.get("Tab") == "Skew"
            and timing.get("Category") in ("RTT", "RON")
        )
    ]

    rtt_rows = [
        _make_dual_live_row(
            "RTT WR", "RTT", get_ddr4_rtt_wr,
        ),
        _make_dual_live_row(
            "RTT NOM", "RTT", get_ddr4_rtt_nom,
        ),
        _make_dual_live_row(
            "RTT PARK", "RTT", get_ddr4_rtt_park,
        ),
    ]
    ron_rows = [
        _make_dual_live_row(
            "PULL UP", "RON", get_ddr4_ron,
        ),
        _make_dual_live_row(
            "PULL DN", "RON", get_ddr4_ron,
        ),
    ]

    # Preserve the original Skew order: RTT, ODT, RON, then the remaining sections.
    first_skew = next(
        (i for i, timing in enumerate(TIMINGS) if timing.get("Tab") == "Skew"),
        len(TIMINGS),
    )
    TIMINGS[first_skew:first_skew] = rtt_rows

    after_odt = first_skew + len(rtt_rows)
    while (
        after_odt < len(TIMINGS)
        and TIMINGS[after_odt].get("Tab") == "Skew"
        and TIMINGS[after_odt].get("Category") == "ODT"
    ):
        after_odt += 1
    TIMINGS[after_odt:after_odt] = ron_rows


_install_ddr4_skew_live_rows()


# --- UI order adjustment for Main -> Secondary timings.
def _reorder_main_secondary_timings():
    global TIMINGS
    desired_order = [
        "tWR", "tRFC", "tRFCpb", "tRRD_L", "tRRD_S",
        "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL",
    ]

    secondary_rows = [
        t for t in TIMINGS
        if t.get("Tab") == "Main" and t.get("Category") == "Secondary"
    ]
    if not secondary_rows:
        return

    desired_index = {name: i for i, name in enumerate(desired_order)}

    ordered_secondary = sorted(
        secondary_rows,
        key=lambda t: (
            0 if t.get("name") in desired_index else 1,
            desired_index.get(t.get("name"), 999),
            secondary_rows.index(t),
        )
    )

    result = []
    inserted = False
    for timing in TIMINGS:
        if timing.get("Tab") == "Main" and timing.get("Category") == "Secondary":
            if not inserted:
                result.extend(ordered_secondary)
                inserted = True
            continue
        result.append(timing)

    TIMINGS = result


_reorder_main_secondary_timings()


# --- DDR4-aware ODT section.
# The CA/CS/CK Group A/B fields are DDR5-only. On DDR4 the live DRAM ODT
# settings are the DQ/DQS RTT_NOM, RTT_WR, and RTT_PARK values.
def _install_ddr4_odt_rows():
    global TIMINGS
    if detect_ddr_generation() != "DDR4":
        return

    # Remove the DDR5-only CA/CS/CK ODT rows that otherwise return N/A.
    TIMINGS = [
        timing for timing in TIMINGS
        if not (
            timing.get("Tab") == "Skew"
            and timing.get("Category") == "ODT"
        )
    ]

    odt_rows = [
        _make_dual_live_row(
            "DQ/DQS ODT NOM", "ODT", get_ddr4_rtt_nom,
        ),
        _make_dual_live_row(
            "DQ/DQS ODT WR", "ODT", get_ddr4_rtt_wr,
        ),
        _make_dual_live_row(
            "DQ/DQS ODT PARK", "ODT", get_ddr4_rtt_park,
        ),
    ]

    # Keep the Skew order as RTT -> ODT -> RON.
    insert_at = next(
        (
            i for i, timing in enumerate(TIMINGS)
            if timing.get("Tab") == "Skew"
            and timing.get("Category") == "RON"
        ),
        next(
            (i for i, timing in enumerate(TIMINGS) if timing.get("Tab") == "Skew"),
            len(TIMINGS),
        ),
    )
    TIMINGS[insert_at:insert_at] = odt_rows


_install_ddr4_odt_rows()

# --- Core Ultra 200S / Z890 VREF cleanup.
# The legacy client-IMC drive-strength block used by Alder/Raptor Lake -- the
# WrDS and RdODT rows at 0x2CE8/0x2CEC/0x2CF0 -- reads as 0xFFFFFFFF on Arrow
# Lake. Do not present those invalid 255 values. Arrow Lake keeps the valid
# DDR5 mode-register VREFDQ/VREFCA/VREFCS rows.
def _install_arrow_lake_vref_rows():
    global TIMINGS
    if not is_arrow_lake_platform():
        return

    # Remove only the obsolete analog drive-strength rows. The three DDR5
    # mode-register values currently live in the "VREF Additional" category.
    TIMINGS = [
        timing for timing in TIMINGS
        if not (
            timing.get("Tab") == "Skew"
            and timing.get("Category") == "VREF"
        )
    ]

    # Present DQ/CA/CS as the normal VREF section on Arrow Lake rather than a
    # second "VREF Additional" panel.
    for timing in TIMINGS:
        if (
            timing.get("Tab") == "Skew"
            and timing.get("Category") == "VREF Additional"
            and timing.get("name") in ("DQ VREF", "CA VREF", "CS VREF")
        ):
            timing["Category"] = "VREF"


_install_arrow_lake_vref_rows()

# --- Alder/Raptor Lake memory I/O slew-rate and compensation results.
# These are trained PHY/compensation results exposed through the client MCHBAR.
# Arrow Lake uses a different register layout; all-ones/unsupported registers are
# reported as N/A rather than displaying misleading 63/255 values.
def _read_slew_rate_field(offset, bit_start, bit_length):
    try:
        dword = read_physical_memory_int(MCHBAR + offset, 4)
        if dword is None or int(dword) == 0xFFFFFFFF:
            return "N/A"
        mask = (1 << bit_length) - 1
        return str((int(dword) >> bit_start) & mask)
    except Exception as e:
        print(f"Error reading slew-rate field at 0x{offset:X}: {e}")
        return "N/A"


# Skew rows that hold text rather than a reading, and are meant to. Both are
# DDR4: VREFCA is an external reference the controller cannot report, and
# DDR4 has no VREFCS register at all, so there is nothing to read in either
# case. They say so instead of showing a number that would look measured.
# Everything else on the tab must reach hardware when it is drawn -- see
# tests/test_skew_live.py, which allows exactly these two.
SKEW_FIXED_BY_SPECIFICATION = frozenset({"CA VREF", "CS VREF"})


# The slew-rate rows read in five groups -- the four signal classes and the
# common SComp block -- and the tab draws a category as its own heading, so
# splitting them here is what puts those headings on screen. The row order
# inside each group is unchanged; only the grouping is new.
SLEW_RATE_GROUPS = ("DATA", "CMD", "CLK", "CTL", "SComp")


# Named rather than prefixed: this row starts with "CMD" but belongs to the
# common SComp block, and grouping it by its prefix split that block in two.
SLEW_RATE_SCOMP_ROWS = frozenset({"CMD SlewStatlegen"})


def _slew_rate_category(name):
    """Which slew-rate group a row belongs to, from the name it already has.

    Matched without regard to case. The rows carry the reference tools' own
    names, and theirs is "Data Drv Up" against a heading of DATA; an exact
    prefix test dropped every DATA row into the SComp group the moment the
    names were aligned.
    """
    if name in SLEW_RATE_SCOMP_ROWS:
        return "SComp"
    for group in ("DATA", "CMD", "CLK", "CTL"):
        if name.upper().startswith(group + " "):
            return group
    return "SComp"


# Every row on this tab holds a getter rather than a number. These registers
# are live compensation results that retrain while the tool is open -- the
# VssHiFF fields move between 48 and 49, and CLK's pull-down tracks them --
# so a value read once at import would freeze at whatever the state happened
# to be at startup and never move again. That is what made the tab appear to
# disagree with the reference tools: both were right, ours was just older.
def _make_slew_rate_row(name, offset, bit_start, bit_length):
    return {
        "name": name,
        "Category": _slew_rate_category(name),
        "Tab": "Skew",
        "Column": "Right",
        "value": (lambda offset=offset, bit_start=bit_start,
                  bit_length=bit_length:
                  _read_slew_rate_field(offset, bit_start, bit_length)),
    }


def _make_slew_rate_row_for_generation(name, offset, bit_start, bit_length):
    """A slew row whose base depends on the DDR generation.

    The generation is resolved once, here, and the reader it selects is what
    gets called each refresh. Probing the generation on every read would put
    a WMI round trip on the refresh path for a fact that cannot change while
    the tool is running.
    """
    if detect_ddr_generation() == "DDR4":
        read = (lambda: _read_slew_rate_field_from_base(
            MCHBAR, offset, bit_start, bit_length))
    else:
        read = lambda: _read_slew_rate_field(offset, bit_start, bit_length)
    return {
        "name": name,
        "Category": _slew_rate_category(name),
        "Tab": "Skew",
        "Column": "Right",
        "value": read,
    }


def _read_slew_rate_field_from_base(base, offset, bit_start, bit_length):
    """Read one slew-rate field from an explicitly selected controller base."""
    try:
        dword = read_physical_memory_int(base + offset, 4)
        if dword is None or int(dword) == 0xFFFFFFFF:
            return "N/A"
        mask = (1 << bit_length) - 1
        return str((int(dword) >> bit_start) & mask)
    except Exception as e:
        print(f"Error reading slew-rate field at 0x{base + offset:X}: {e}")
        return "N/A"


def _read_ddr4_clk_slew_field(field_name):
    """Read Z690/Z790 DDR4 clock compensation from the live FEDC block.

    The Z790 DDR4 CLK result is not laid out like DATA/CMD/CTL at 0x2CE0.
    The verified dump stores the trained clock codes in the split clock
    compensation registers at 0x2CFC/0x2CF4.
    """
    try:
        if detect_ddr_generation() != "DDR4":
            # CLK shares a compensation domain with CTL, in 0x2CE4. Every one
            # of its fields comes from that register, the pull-down included.
            #
            # This read 0x2CE0 before, on the reasoning that DATA/CMD/CLK/CTL
            # ran 0x2CD8/0x2CDC/0x2CE0/0x2CE4 and CLK therefore had to be the
            # third. Position was never evidence. CMD and CTL hold an
            # identical low word -- 0x04D9, giving 25 and 19 -- because the
            # command-group signals share their drive compensation, while
            # 0x2CE0 holds 0x3C7D, a different shape whose drive-up sits
            # pinned at 61 and VssHiFF at 63, the six-bit maximum, in every
            # sample. That is not a register of this family.
            #
            # Widths follow the reference tools. Their CLK block is a 64-bit
            # read at 0x2CE0 with starts of 32, 48, 44 and 52; that upper word
            # is 0x2CE4, so the same fields are 0, 16, 12 and 20 read here.
            #
            # The pull-down deliberately does not follow them. Both tools put
            # it at 48, which is bits 16-21 of this register, and that window
            # is not a field: bits 16-19 are the top nibble of SComp and bits
            # 20-21 are the low end of VssHiFF, so it reads 2 or 18 depending
            # on where VssHiFF happens to be. The five CTL fields already
            # account for all 32 bits -- 6 + 6 + 8 + 6 + 6 -- so there is no
            # room for a separate CLK pull-down, and sweeping 0x2C00-0x2D00
            # for another comp-shaped register turns up only 0x2CDC (CMD).
            #
            # Writing both blocks in the same 64-bit frame shows what the 48
            # is. CLK is 32, 48, 44, 52. CTL, plus 32, is 32, 38, 44, 52, 58.
            # Drive-up, SComp and VssHiFF are identical; only the pull-down
            # differs, 48 against 38, which reads as 38 with a digit wrong.
            # Bit 6 would give 19: steady, and the same value CMD and CTL
            # trained to, which is what a shared domain should produce.
            #
            # Bit 16 is used anyway, by explicit decision: agreeing with the
            # reference tools matters more here than the argument above, and
            # this row is compared against them directly. It reads 18, and it
            # moves to 2 or 50 when SComp or VssHiFF do. Bit 6 is the change
            # if that is ever revisited.
            fields = {
                "up": (0x2CE4, 0, 6),
                "dn": (0x2CE4, 16, 6),
                "scomp": (0x2CE4, 12, 8),
                "vsshiff": (0x2CE4, 20, 6),
            }
        else:
            fields = {
                # Verified against the supplied FEDC0000 dump:
                #   0x2CFC = 0x0320A0FF -> 63 / 50 / 32
                #   0x2CF4 = 0x00A2407F -> VssHiFF 63
                "up": (0x2CFC, 0, 6),
                "dn": (0x2CFC, 20, 6),
                "scomp": (0x2CFC, 8, 6),
                "vsshiff": (0x2CF4, 0, 6),
            }

        offset, bit_start, bit_length = fields[field_name]
        return _read_slew_rate_field_from_base(MCHBAR, offset, bit_start, bit_length)
    except Exception as e:
        print(f"Error reading DDR4 CLK slew field {field_name}: {e}")
        return "N/A"


def _make_ddr4_clk_slew_row(name, field_name):
    return {
        "name": name,
        "Category": _slew_rate_category(name),
        "Tab": "Skew",
        "Column": "Right",
        "value": lambda field_name=field_name: _read_ddr4_clk_slew_field(
            field_name),
    }


def _install_slew_rate_rows():
    global TIMINGS

    # Remove an older copy when this file is reloaded during development.
    TIMINGS = [
        timing for timing in TIMINGS
        if not (
            timing.get("Tab") == "Skew"
            and timing.get("Category") == "Slew Rate"
        )
    ]

    slew_rows = [
        # DATA
        _make_slew_rate_row("Data Drv Up", 0x2CD8, 0, 6),
        _make_slew_rate_row("Data Drv Dn", 0x2CD8, 6, 6),
        _make_slew_rate_row("Data ODT Up", 0x2CD8, 12, 6),
        _make_slew_rate_row("Data ODT Dn", 0x2CD8, 18, 6),
        _make_slew_rate_row_for_generation(
            "Data VssHiFFdq", 0x2CD8, 24, 6),

        # CMD
        _make_slew_rate_row("CMD Drv Up", 0x2CDC, 0, 6),
        _make_slew_rate_row("CMD Drv Dn", 0x2CDC, 6, 6),
        _make_slew_rate_row("CMD SComp", 0x2CDC, 12, 8),
        _make_slew_rate_row_for_generation("CMD VssHiFF", 0x2CDC, 20, 6),

        # CLK - Z790 DDR4 trained result is read from MC1/CHB.
        _make_ddr4_clk_slew_row("CLK Drv Up", "up"),
        _make_ddr4_clk_slew_row("CLK Drv Dn", "dn"),
        _make_ddr4_clk_slew_row("CLK SComp", "scomp"),
        _make_ddr4_clk_slew_row("CLK VssHiFF", "vsshiff"),

        # CTL
        _make_slew_rate_row("CTL Drv Up", 0x2CE4, 0, 6),
        _make_slew_rate_row("CTL Drv Dn", 0x2CE4, 6, 6),
        _make_slew_rate_row("CTL SComp", 0x2CE4, 12, 8),
        _make_slew_rate_row("CTL VssHiFF", 0x2CE4, 20, 6),
        _make_slew_rate_row("CTL CkeCsUp", 0x2CE4, 26, 6),

        # SComp diagnostics
        # One packed register rather than three scattered ones.
        #
        # These used to read from 0x2D00, 0x2CE8 and 0x2CF4. 0x2C24 holds them
        # in one tidy packed layout -- a flag then two byte fields -- and
        # decodes to 1 / 0 / 2 here, matching what the old addresses reported.
        # Three unrelated registers agreeing with one packed one is
        # coincidence; the packed one is the source, and it is what both
        # reference tools name.
        #
        # bonus20 and bonus19 are gone. Both tools place them at bits 22+3 and
        # 25+1 of this register, where they read 0 and 0, and neither is a
        # reading worth a row. bonus20 in particular is why: those tools show
        # 48 for it, but no window anywhere in 0x2C24 holds 48, and their own
        # map gives that row the id "ctlvsshiff" -- the same id as their CTL
        # VssHiFF row. They resolve by id, so the row renders CTL VssHiFF, and
        # CTL VssHiFF is 48. The number never came from this register at all.
        _make_slew_rate_row("CMD SlewStatlegen", 0x2C24, 0, 1),
        _make_slew_rate_row("SComp codelive", 0x2C24, 2, 8),
        _make_slew_rate_row("SComp cmn bonus", 0x2C24, 12, 8),
    ]

    # Place the new panel before MISC Additional in the Skew right column.
    insert_at = next(
        (
            index for index, timing in enumerate(TIMINGS)
            if timing.get("Tab") == "Skew"
            and timing.get("Category") == "MISC Additional"
        ),
        len(TIMINGS),
    )
    TIMINGS[insert_at:insert_at] = slew_rows


_install_slew_rate_rows()

# --- Skew-tab organization cleanup.
# Keep the legacy analog VREF values and the DQ/CA/CS VREF values in one panel,
# order WrDSCke CS Up after WrDSClk Dn, and place ODT DELAY below ODTL.
def _organize_skew_sections():
    global TIMINGS

    vref_name_order = [
        "WrDS Up",
        "WrDS Dn",
        "RdODT Up",
        "RdODT Dn",
        "WrDSCmd Up",
        "WrDSCmd Dn",
        "WrDSCtl Up",
        "WrDSCtl Dn",
        "WrDSClk Up",
        "WrDSClk Dn",
        # CKE/CS is the one rail with an up level and no down level. See the
        # note by the VREF rows: nothing maps a CKE/CS pull-down, and BIOS
        # offers no setting for one.
        "WrDSCke CS Up",
        # The two that are not up/down pairs, kept together directly under the
        # pairs rather than trailing the per-device levels below them.
        "RX VREF",
        "QXCOUNT",
        # DDR5 reports a level per DRAM device; DDR4 trains one per rank and
        # collapses to the single row. Both spellings are listed so the
        # generation that is not running does not sort its row to the end.
        "DQ VREF",
        "DQ VREF D0",
        "DQ VREF D1",
        "DQ VREF D2",
        "DQ VREF D3",
        "CA VREF",
        "CS VREF",
    ]

    vref_rows = [
        timing for timing in TIMINGS
        if timing.get("Tab") == "Skew"
        and timing.get("Category") in ("VREF", "VREF Additional")
    ]
    if vref_rows:
        first_vref_index = min(TIMINGS.index(timing) for timing in vref_rows)
        for timing in vref_rows:
            timing["Category"] = "VREF"
            timing["Column"] = "Right"

        order_lookup = {name: index for index, name in enumerate(vref_name_order)}
        original_lookup = {id(timing): index for index, timing in enumerate(vref_rows)}
        vref_rows.sort(
            key=lambda timing: (
                order_lookup.get(timing.get("name"), len(order_lookup)),
                original_lookup[id(timing)],
            )
        )
        TIMINGS = [timing for timing in TIMINGS if timing not in vref_rows]
        TIMINGS[first_vref_index:first_vref_index] = vref_rows

    odt_delay_rows = [
        timing for timing in TIMINGS
        if timing.get("Tab") == "Skew"
        and timing.get("Category") == "ODT DELAY"
    ]
    if odt_delay_rows:
        TIMINGS = [timing for timing in TIMINGS if timing not in odt_delay_rows]
        for timing in odt_delay_rows:
            timing["Column"] = "Left"
        odtl_indices = [
            index for index, timing in enumerate(TIMINGS)
            if timing.get("Tab") == "Skew"
            and timing.get("Category") == "ODTL"
        ]
        insert_at = (max(odtl_indices) + 1) if odtl_indices else len(TIMINGS)
        TIMINGS[insert_at:insert_at] = odt_delay_rows


_organize_skew_sections()


# --- DDR4 ODT latency values.
# The old ODTL reader searched a legacy MRC command table that is not present
# on the tested Z790 DDR4 memory-controller dump, which produced N/A or stale
# offset codes.  For DDR4, expose the actual JEDEC latencies derived from the
# active controller tCL/tCWL values (desktop UDIMM defaults: AL=0, PL=0, 1tCK
# preamble).  Keep the existing trained-offset reader on DDR5 platforms.
def _ddr4_odtl_clocks(kind):
    """One DDR4 ODT latency, derived from the live tCL/tCWL pair.

    Derived on each call rather than once while the table is built. The two
    timings it depends on are themselves read from the controller, and a row
    that quotes them has to move when they do.
    """
    try:
        tcl = read_timing(
            MCHBAR + 0xE070,
            bit_start=16,
            bit_length=7,
            read_type="standard",
        )
        tcwl = read_timing(
            MCHBAR + 0xE070,
            bit_start=24,
            bit_length=7,
            read_type="standard",
        )
        if tcl is None or tcwl is None:
            return "N/A"

        write = max(0, int(tcwl) - 2)
        read_off = max(0, int(tcl) - 2)
        clocks = {
            "write_on": write,
            "write_off": write,
            "read_on": read_off + 6,
            "read_off": read_off,
        }[kind]
        return f"{clocks} Clocks"
    except Exception as e:
        print(f"Error calculating DDR4 ODTL values: {e}")
        return "N/A"


def _ddr4_odtl_row(name, kind):
    return {
        "name": name,
        "value": lambda kind=kind: _ddr4_odtl_clocks(kind),
        "Category": "ODTL",
        "Tab": "Skew",
        "Column": "Left",
        "read_type": "standard",
    }


def _install_ddr4_odtl_values():
    global TIMINGS
    if detect_ddr_generation() != "DDR4":
        return

    old_rows = [
        timing for timing in TIMINGS
        if timing.get("Tab") == "Skew" and timing.get("Category") == "ODTL"
    ]
    if not old_rows:
        return
    insert_at = min(TIMINGS.index(timing) for timing in old_rows)
    TIMINGS = [timing for timing in TIMINGS if timing not in old_rows]

    replacement_rows = [
        _ddr4_odtl_row("ODTL WR ON", "write_on"),
        _ddr4_odtl_row("ODTL WR OFF", "write_off"),
        _ddr4_odtl_row("ODTL WR NT ON", "write_on"),
        _ddr4_odtl_row("ODTL WR NT OFF", "write_off"),
        _ddr4_odtl_row("ODTL RD NT ON", "read_on"),
        _ddr4_odtl_row("ODTL RD NT OFF", "read_off"),
    ]
    TIMINGS[insert_at:insert_at] = replacement_rows


_install_ddr4_odtl_values()


# --- DDR4 refresh-mode fix.
# Z790 DDR4 does not expose the old MRC command-table record used by the
# original dynamic reader. Replace only that row with Intel DDR_PTM_CTL[3:2].
def _install_ddr4_refresh_mode_value():
    if detect_ddr_generation() != "DDR4":
        return
    for timing in TIMINGS:
        if (
            timing.get("Tab") == "Skew"
            and timing.get("Category") == "REFRESH MODE"
            and timing.get("name") == "REFRESH"
        ):
            timing.clear()
            timing.update({
                "name": "REFRESH",
                "value": get_ddr4_refresh_mode_value,
                "Category": "REFRESH MODE",
                "Tab": "Skew",
                "Column": "Left",
                "read_type": "standard",
            })
            break


_install_ddr4_refresh_mode_value()

# --- v46 tab organization.
# Keep all platform-specific installers working with the historical "Main"
# label, then remap the completed rows once the timing table is finalized.
def _install_system_info_and_timings_tabs():
    for timing in TIMINGS:
        if timing.get("Tab") != "Main":
            continue
        if timing.get("Category") == "General":
            timing["Tab"] = "System Info"
        else:
            timing["Tab"] = "Timings"


_install_system_info_and_timings_tabs()


# --- Core Ultra 200S power-down fields.
# 0xE050 carries tRDPDEN and tCPDED at different bit positions than
# Alder/Raptor Lake, confirmed by diffing full MCHBAR snapshots across a BIOS
# change (tRDPDEN 34->61, tCPDED 22->23) and reproduced identically in all
# four channel copies at 0xE050/0xE850/0xF050/0x1E050.
# tWRPDEN and tPRPDEN stay unresolved: nothing in the memory-controller block
# tracked tWRPDEN 90->123, and the only tPRPDEN candidate overlaps the
# confirmed tRDPDEN bits, so both report N/A rather than a wrong number.
ARROW_LAKE_POWER_DOWN = {
    "tRDPDEN": (0xE050, 19, 6),
    "tCPDED": (0xE050, 14, 5),
}
ARROW_LAKE_POWER_DOWN_UNKNOWN = ("tWRPDEN", "tPRPDEN")

# --- DEC_TCWL.
#
# The write-leveling decrement applied to tCWL, per channel. Two things are
# established and one is not, and the row is written to survive the third
# turning out otherwise.
#
# Established: the register. The reference tool reads exactly three registers
# this project does not, and this is one of them. The code around that read
# emits exactly three field descriptors, and the group it draws holds exactly
# three fields -- DEC_TCWL, ADD_TCWL and ADD_1QCLK_DELAY. Three against three
# is not a coincidence the way a single matching value would be.
#
# Established: the value. Channel A reads 3 and channel B reads 4 on the
# LGA1700 DDR5 bench, and the reference tool shows 4 -- the channel-B figure,
# since it draws one value where this shows both. A trained per-channel
# adjustment differing by one across a matched kit is what that should look
# like.
#
# Not established: the field width. The other two fields in the register read
# zero, so the whole register is this value and any width from 3 bits up
# reads the same. Four is taken from the control's own bound of 15. If a
# future BIOS drives DEC_TCWL past 15 this truncates, which is the one way
# the row can be wrong -- and the neighbouring fields going non-zero would
# show it, since the register would stop equalling the reading.
#
# Not carried to Arrow Lake: that platform moved several registers in this
# block, so the same offset there would read a plausible number meaning
# something else. It reports N/A until someone establishes it there.
DEC_TCWL_OFFSET = 0xE478
DEC_TCWL_ROW = "DEC_TCWL"


def _install_arrow_lake_power_down_rows():
    if not is_arrow_lake_platform():
        return

    for timing in TIMINGS:
        name = timing.get("name")
        if name in ARROW_LAKE_POWER_DOWN:
            offset, bit_start, bit_length = ARROW_LAKE_POWER_DOWN[name]
            timing["address"] = MCHBAR + offset
            timing["parameters"] = {
                "bit_start": bit_start,
                "bit_length": bit_length,
            }
            timing["read_type"] = "standard"
            timing.pop("value", None)
            timing.pop("Formula", None)
        elif name in ARROW_LAKE_POWER_DOWN_UNKNOWN or name == DEC_TCWL_ROW:
            timing["value"] = "N/A"
            timing["read_type"] = "standard"
            timing.pop("address", None)
            timing.pop("parameters", None)
            timing.pop("Formula", None)


_install_arrow_lake_power_down_rows()


# --- Derived refresh row.
# tRFC (ns) is computed from the live tRFC and MCLK, mirroring AM5, and is
# pinned directly above tRFC so Summary and Timings share one order.
def _install_trfc_ns_row():
    index = next(
        (
            i for i, timing in enumerate(TIMINGS)
            if timing.get("name") == "tRFC"
            and timing.get("Category") == "Refresh timings"
        ),
        None,
    )
    if index is None:
        return
    if any(timing.get("name") == "tRFC (ns)" for timing in TIMINGS):
        return

    reference = TIMINGS[index]
    TIMINGS.insert(index, {
        "name": "tRFC (ns)",
        "value": get_trfc_ns,
        "Category": "Refresh timings",
        "Tab": reference.get("Tab", "Timings"),
        "Column": reference.get("Column", "Left"),
        "read_type": "standard",
    })


_install_trfc_ns_row()


def _install_trefi_ns_row():
    """Pin tREFI (ns) directly below the raw interval it restates."""
    index = next(
        (
            i for i, timing in enumerate(TIMINGS)
            if timing.get("name") == "tREFI"
            and timing.get("Category") == "Refresh timings"
        ),
        None,
    )
    if index is None:
        return
    if any(timing.get("name") == "tREFI (ns)" for timing in TIMINGS):
        return

    reference = TIMINGS[index]
    TIMINGS.insert(index + 1, {
        "name": "tREFI (ns)",
        "value": get_trefi_ns,
        "Category": "Refresh timings",
        "Tab": reference.get("Tab", "Timings"),
        "Column": reference.get("Column", "Left"),
        "read_type": "standard",
    })


_install_trefi_ns_row()


# --- Column-to-column delays.
#
# tCCD_L and tCCD_L_WR used to be read by searching the mode-register window
# for a DDR5 MR pattern and decoding the hit through a JEDEC MR table. That
# search finds nothing on DDR4, and its zero result indexed the tables' first
# entry, so tCCD_L printed a constant 8 that happened to be right and
# tCCD_L_WR printed 32, a DDR5 figure with no meaning on DDR4. Neither was a
# reading, so both are gone.
#
# What replaces them is not yet a reading either: see CCD_CONFIRMED_FIELDS for
# which register field was tried, how a controlled BIOS change refuted it, and
# what would confirm the remaining candidate. Until then these rows report
# nothing, rather than the number the refuted mapping would print.
#
# The delay the controller actually applies is not hidden by this: it is on
# the same tab as tRDRD_sg, tRDRD_dg and tWRWR_sg.
#
# Runs before the channel-B pass so these rows gain the same A1/B1 columns as
# the rest of the tab.

# tCCD is deliberately absent. DDR5 fixes the different-bank-group delay at
# 8 nCK and no mode register carries it, so the row could only ever be blank
# or be filled with the constant as though it had been read.
CCD_ROWS = (
    ("tCCD_L", lambda base=None: get_ccd_timing("tCCD_L", base)),
    ("tCCD_L_WR", lambda base=None: get_ccd_timing("tCCD_L_WR", base)),
    ("tCCD_L_WR2", lambda base=None: get_ccd_timing("tCCD_L_WR2", base)),
)

# What the mode-register search left behind, in the order they appeared.
SUPERSEDED_CCD_ROWS = ("tCCDL", "tCCDL WR")

# Where the delays live now, and the row they follow.
CCD_CATEGORY = "Other Timings"
CCD_ANCHOR = "tCAL"


def _install_dfe_rows():
    """Put the four DFE taps on the Skew tab, directly below ODT DELAY.

    Per channel: the mode-register table exists in both controller windows and
    the taps genuinely differ between them, so each row reads its own side
    rather than showing one controller's training twice.

    DDR4 gets no rows at all. Decision feedback equalisation is a DDR5 feature
    -- the equaliser sits in the DRAM's DQ receiver and is configured through
    mode registers that DDR4 does not define -- so there is nothing to read
    rather than something that failed to read. Measured on the Z790-P DDR4
    bench, the mode-register window is empty in any case: not one entry in the
    whole 0xE600-0xE800 table, which is also why tCCD_L reports nothing there.
    """
    global TIMINGS

    # Scoped to this tab. Removing every row categorised DFE also took the
    # Jedec tab's global rows, which are a different section that happens to
    # share the name -- and took them before anything could decide whether to
    # keep them.
    TIMINGS = [
        timing for timing in TIMINGS
        if not (timing.get("Tab") == "Skew" and timing.get("Category") == "DFE")
    ]

    if detect_ddr_generation() == "DDR4":
        return

    anchor = None
    column = "Right"
    for index, timing in enumerate(TIMINGS):
        if timing.get("Tab") == "Skew" and timing.get("Category") == "ODT DELAY":
            anchor = index + 1
            column = timing.get("Column", column)

    rows = []
    for tap in range(1, DFE_TAP_COUNT + 1):
        for suffix, getter in (
            ("Enable", get_dfe_enable),
            ("Bias", get_dfe_bias),
        ):
            rows.append({
                "name": "DFE Tap %d %s" % (tap, suffix),
                "value_a": (lambda tap=tap, getter=getter:
                            getter(tap, MCHBAR)),
                "value_b": (lambda tap=tap, getter=getter:
                            getter(tap, CHANNEL_B)),
                "Category": "DFE",
                "Tab": "Skew",
                "Column": column,
                "read_type": "standard",
            })
    if anchor is None:
        TIMINGS.extend(rows)
    else:
        TIMINGS[anchor:anchor] = rows


_install_dfe_rows()


# The Jedec tab held nothing but DFE rows -- the per-tap enables and values,
# and the global gain -- and the taps now sit on the Skew tab beside the VREF
# levels they bias. A whole tab for one section that lives somewhere else is a
# second place to look for the same thing, so it goes.
#
# The gain is the exception. It is not a tap, so the per-tap rows above do not
# cover it, and dropping the tab would have taken the only reading of it with
# them. These two move rather than being rebuilt, so their decode and formulas
# come across exactly as they were.
JEDEC_TAB = "Jedec"

JEDEC_ROWS_KEPT = {
    "Global DFE Gain": "DFE Gain Enable",
    "DFE GAIN Value": "DFE Gain",
}


def _remove_jedec_tab():
    global TIMINGS

    # The gain describes the same DDR5 equaliser the taps do, so where the taps
    # are not installed it is not carried across either -- it would be the only
    # DFE row on a generation that has no DFE.
    keep_dfe = detect_ddr_generation() != "DDR4"

    kept = []
    remaining = []
    column = "Right"
    last_dfe = None
    for timing in TIMINGS:
        if timing.get("Tab") == "Skew" and timing.get("Category") == "DFE":
            column = timing.get("Column", column)
        if timing.get("Tab") != JEDEC_TAB:
            remaining.append(timing)
            if timing.get("Tab") == "Skew" and timing.get("Category") == "DFE":
                last_dfe = len(remaining)
            continue
        label = JEDEC_ROWS_KEPT.get(timing.get("name")) if keep_dfe else None
        if label is None:
            continue
        moved = dict(timing)
        moved["name"] = label
        moved["Tab"] = "Skew"
        moved["Category"] = "DFE"
        moved["Column"] = column
        kept.append(moved)

    if last_dfe is None:
        remaining.extend(kept)
    else:
        remaining[last_dfe:last_dfe] = kept
    TIMINGS = remaining


_remove_jedec_tab()


def _place_misc_additional_last():
    """Put the MISC Additional rows after DFE, at the foot of its column.

    They are the leftovers of the tab -- bandwidth select, the DCC code, the
    ODT enables -- so they read as a footnote rather than as a section between
    the signal groups.
    """
    global TIMINGS

    misc = [
        timing for timing in TIMINGS
        if timing.get("Tab") == "Skew"
        and timing.get("Category") == "MISC Additional"
    ]
    if not misc:
        return

    rest = [timing for timing in TIMINGS if timing not in misc]
    anchor = None
    column = "Right"
    for index, timing in enumerate(rest):
        if timing.get("Tab") == "Skew" and timing.get("Category") == "DFE":
            anchor = index + 1
            column = timing.get("Column", column)
    for timing in misc:
        timing["Column"] = column
    if anchor is None:
        rest.extend(misc)
    else:
        rest[anchor:anchor] = misc
    TIMINGS = rest


_place_misc_additional_last()


def _install_ccd_timings():
    """Put the column-to-column delays at the foot of the other timings.

    Placed after CCD_ANCHOR rather than where the rows they replaced sat: a
    section draws its rows in table order, so leaving them among the
    secondaries would have put them at the head of their new section rather
    than at its foot.
    """
    for name in SUPERSEDED_CCD_ROWS:
        for timing in list(TIMINGS):
            if timing.get("name") == name:
                TIMINGS.remove(timing)

    rows = [
        {
            "name": name,
            "value": getter,
            "Category": CCD_CATEGORY,
            "Tab": "Timings",
            "Column": "Right",
            "read_type": "standard",
        }
        for name, getter in CCD_ROWS
    ]

    anchor = next(
        (index + 1 for index, timing in enumerate(TIMINGS)
         if timing.get("name") == CCD_ANCHOR
         and timing.get("Category") == CCD_CATEGORY),
        None,
    )
    if anchor is None:
        TIMINGS.extend(rows)
    else:
        TIMINGS[anchor:anchor] = rows


_install_ccd_timings()


# --- Channel B columns on the Timings tab.
#
# Every row above reads the first memory controller at MCHBAR, so the tab has
# only ever shown channel A. The second controller repeats the same register
# block one 0x10000 window higher: the RTL rows already depend on that when
# they label MC0 as A1/A2 and MC1 as B1/B2, and the Arrow Lake power-down
# survey found tRDPDEN identical at 0xE050 and 0x1E050, which is that same
# window delta. Promoting a row therefore means reading one offset from both
# controllers and letting the section draw the ChA/ChB columns main.py already
# renders for AM5.
#
# This runs after every other installer so it mirrors the addresses those
# passes finalised rather than the ones the table literal declared. Anything it
# cannot mirror from a confirmed MC0 address is left single, so an unmirrored
# row shows one value instead of a register that was never verified to hold
# that timing.

MC_WINDOW = MCHBAR2 - MCHBAR

DUAL_CHANNEL_TAB = "Timings"

# Header text for the two value columns, named for the slots actually
# populated on each controller rather than assumed to be the first of each
# pair. The board reports its own slot names, and on this target they are A2
# and B2, not A1 and B1 - so a fixed label would have named the wrong sockets
# and disagreed with the module strip along the bottom of the window.
#
# A channel with two modules is named for both, "A1/A2". A channel whose slot
# the board did not name falls back to the controller, which is what the column
# always really is.
def _channel_labels():
    try:
        from dimm_inventory import read_modules, slots_by_channel

        channels = slots_by_channel(read_modules())
    except Exception:
        channels = {}
    return (
        "/".join(channels.get("A", ())) or "ChA",
        "/".join(channels.get("B", ())) or "ChB",
    )


CHANNEL_A_LABEL, CHANNEL_B_LABEL = _channel_labels()

# Capacity, Speed and Channels describe the system, and the ratio, gear and
# stretch rows describe a configuration both controllers run. A second column
# would repeat the first rather than tell a tuner anything, so these stay
# single and the section renders them value-then-blank.
DUAL_CHANNEL_SKIP_CATEGORIES = ("General",)

# Rows with no address to mirror. Each side names the getter to call for its
# controller, bound per read rather than at import, so channel B is read the
# same way channel A is.
DUAL_CHANNEL_COMPUTED = {
    "tRC": get_trc_value,
    "CR": get_command_rate,
    "tWTR_L": get_tWTR_L,
    "tWTR_S": get_tWTR_S,
    # Derived rather than read: the platform passes replace both of these with
    # a getter and drop the address, so there is nothing for the address
    # mirror to work from.
    "tWR": get_twr_value,
    "tRFC (ns)": get_trfc_ns,
    "tREFI (ns)": get_trefi_ns,
    # Same shape: installed as getters just above, over a register field the
    # mirror cannot see because the row carries no address.
    "tCCD_L": lambda base=None: get_ccd_timing("tCCD_L", base),
    "tCCD_L_WR": lambda base=None: get_ccd_timing("tCCD_L_WR", base),
    "tCCD_L_WR2": lambda base=None: get_ccd_timing("tCCD_L_WR2", base),
    "tDLLK": get_dllk_timing,
}


def _channel_b_address(address):
    """Return the channel-B twin of a channel-A address, or None if there is none.

    How far the twin sits depends on the generation, which is why the distance
    comes from CHANNEL_B_OFFSET rather than being written here: a sub-channel
    on DDR5, the second controller on DDR4. See the note beside it.

    Every row this promotes sits in the 0xE000 block, which is where the RTL
    rows establish the twin. An address already in channel B, one a platform
    pass cleared to None, and anything outside the window all return None so
    the caller leaves the row alone.
    """
    if not isinstance(address, int) or isinstance(address, bool):
        return None
    if not MCHBAR <= address < MCHBAR + MC_WINDOW:
        return None
    if (address - MCHBAR) & CHANNEL_B_OFFSET:
        return None
    return address + CHANNEL_B_OFFSET


def _dual_channel_headers(timing):
    """Label the two value columns and blank the header over the name gutter."""
    timing["parameter_name"] = "Name"
    timing["name_a"] = CHANNEL_A_LABEL
    timing["name_b"] = CHANNEL_B_LABEL


def _promote_standard_row(timing):
    """Mirror a plain address+bitfield row onto the second controller."""
    address_b = _channel_b_address(timing.get("address"))
    if address_b is None:
        return False
    parameters = timing.get("parameters") or {}
    if "bit_start" not in parameters or "bit_length" not in parameters:
        return False

    timing["address_a"] = timing["address"]
    timing["address_b"] = address_b
    timing["parameters_a"] = dict(parameters)
    timing["parameters_b"] = dict(parameters)
    timing["read_type_a"] = "standard"
    timing["read_type_b"] = "standard"
    _dual_channel_headers(timing)
    return True


def _promote_dynamic_row(timing):
    """Mirror a dynamic-search row by pointing its copy at the second MCHBAR."""
    params = timing.get("dynamic_params")
    if not isinstance(params, dict) or params.get("mchbar") != MCHBAR:
        return False

    params_b = dict(params)
    params_b["mchbar"] = CHANNEL_B
    timing["dynamic_params_a"] = dict(params)
    timing["dynamic_params_b"] = params_b
    timing["read_type_a"] = "dynamic"
    timing["read_type_b"] = "dynamic"
    _dual_channel_headers(timing)
    return True


def _promote_computed_row(timing, getter):
    """Give a computed row one getter call per controller."""
    timing["value_a"] = lambda fn=getter: fn(MCHBAR)
    timing["value_b"] = lambda fn=getter: fn(CHANNEL_B)
    _dual_channel_headers(timing)
    return True


def _install_dual_channel_timings():
    for timing in TIMINGS:
        if timing.get("Tab") != DUAL_CHANNEL_TAB:
            continue
        if timing.get("Category") in DUAL_CHANNEL_SKIP_CATEGORIES:
            continue
        if is_dual_timing(timing):
            # A row that already reads both sides its own way keeps doing so.
            continue

        computed = DUAL_CHANNEL_COMPUTED.get(timing.get("name"))
        if computed is not None:
            _promote_computed_row(timing, computed)
            continue
        if timing.get("read_type") == "dynamic":
            _promote_dynamic_row(timing)
            continue
        _promote_standard_row(timing)


_install_dual_channel_timings()


# --- Sensors tab.
#
# Voltages are live readings, unlike everything on the Timings tab, which
# cannot change without a reboot. They were previously stranded: VCCSA and
# VDDQ TX sat at the bottom of System Info among configuration rows, and the
# SA/TX Voltage copies on the CPU tab were never displayed at all, because
# "CPU" is not one of the tabs select_tab_names renders. Both move here, onto
# the same Sensors tab the AM5 profile uses.
#
# The rows carry a callable rather than a value, so the one-second refresh in
# main.py re-reads them instead of showing whatever was true at startup.

SENSOR_TAB = "Sensors"


def _board_rail(key):
    """Read one Super I/O rail, importing that path only when it is used."""
    try:
        from intel_board_sensors import rail_text

        return rail_text(key)
    except Exception:
        return None


def _dimm_temperature(channel):
    """Read one channel's DIMM temperature, importing SMBus only when used.

    Where the sensor lives depends on the memory generation, so both are
    tried. A DDR4 module carries a separate JC-42.4 sensor at 0x18-0x1F; on
    DDR5 that part is gone and the sensor moved inside the SPD5 hub at
    0x50-0x57. Sweeping 0x18-0x1F on a DDR5 board finds nothing at all, which
    is why these two rows read blank on one.

    DDR5 is tried first because its answer is the identified one: the hub is
    asked to confirm it is a hub before its reading is used, while a JC-42.4
    sensor has no such handshake.
    """
    celsius = _ddr5_dimm_temperature(channel)
    if celsius is not None:
        return f"{celsius:.1f} °C"
    try:
        from ddr4_tsod import temperature_text

        return temperature_text(channel)
    except Exception:
        return None


def _ddr5_dimm_temperature(channel):
    """Read one channel's SPD5 hub sensor, or None when there is no hub."""
    try:
        from ddr5_pmic import read_dimm_temperatures
        from intel_pch_smbus import (
            CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES, PchSmbusReader,
        )

        return read_dimm_temperatures(
            PchSmbusReader, CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES
        ).get(channel)
    except Exception:
        return None


def _board_temperature(key):
    """Read one board sensor, importing those paths only when they are used.

    The Super I/O first, then the ACPI EC for what it does not carry. On this
    bench the VRM channel is absent from the Super I/O entirely and lives only
    on the EC, which is why that row was blank rather than the board lacking
    the sensor.
    """
    try:
        from intel_board_sensors import temperature_text

        text = temperature_text(key)
        if text:
            return text
    except Exception:
        pass
    return _ec_temperature(key)


def _ec_temperature(key):
    """Read one sensor from the ACPI EC, on boards whose map is known.

    Gated on the board vendor: the EC protocol is standard but the register
    map is not, and an unknown vendor's byte at the same address would decode
    to a plausible temperature rather than an error.
    """
    try:
        from asus_ec import is_asus_board, temperature_text

        if not is_asus_board(get_motherboard_display()):
            return None
        return temperature_text(key)
    except Exception:
        return None


def _package_power(domain):
    """Read one RAPL domain, importing that path only when it is used."""
    try:
        from intel_rapl import power_text

        return power_text(domain)
    except Exception:
        return None


def _dram_rail(key):
    """Read one DRAM rail from the DIMM's own PMIC over the PCH SMBus.

    The decode is JESD301 and shared with the AM5 path; only the bus differs,
    so this passes the Intel transport and its address lists into the same
    reader rather than duplicating the VID arithmetic.

    Nothing to do on DDR4, where the module has no PMIC and its rails come
    from the board. Asked anyway, the reader sweeps 0x48-0x4F looking for one:
    0x4F answers on the Z790-P bench, it is not a PMIC, and its registers
    decoded to a VPP of 1.500 V -- a value DDR4's 2.5 V rail cannot hold, read
    off a device that was never asked what it was. The rows are absent on that
    generation, so this is the second line rather than the first; a sweep that
    can find a wrong answer should not be run at all.
    """
    if detect_ddr_generation() == "DDR4":
        return None
    try:
        from ddr5_pmic import read_dram_rails
        from intel_pch_smbus import (
            CONTROLLER_OFFSETS, PMIC_ADDRESSES, PchSmbusReader,
        )

        volts = read_dram_rails(
            PchSmbusReader, CONTROLLER_OFFSETS, PMIC_ADDRESSES
        ).get(key)
        return None if volts is None else f"{volts:.3f}V"
    except Exception:
        return None


# VCCSA and VDDQ TX come from the memory controller; DRAM and CPU AUX are
# board measurements that only the Super I/O reports. The temperatures come
# from the modules themselves over SMBus, and are the only readings in the
# Intel path that do not originate in the CPU or the board.
#
# A DDR4 module's thermal sensor is optional. A kit built without one does not
# answer, and that row reports nothing rather than a substitute.
def _core_clock(key):
    """One core clock from the performance counters, or None."""
    try:
        from cpu_clocks import clock_text

        return clock_text(key)
    except Exception:
        return None


# The row the per-processor breakdown hangs under, and the key that marks a
# row as one of those children. The telemetry window folds them away behind
# the parent: sixteen logical processors is a useful thing to be able to open
# and a poor thing to have to scroll past to reach the temperatures.
CORE_EFFECTIVE_CLOCK_ROW = "Core Effective Clock"
PARENT_ROW_KEY = "Parent"


def _per_core_clock_rows(category, column):
    """One row per logical processor, folded under the aggregate row.

    Built at import from what the counters actually enumerate rather than
    from a core count: a machine whose counters do not answer gets no
    children, and the parent row stands alone exactly as it did before.
    """
    try:
        from cpu_clocks import core_clock_text, core_labels

        labels = core_labels()
    except Exception:
        return []

    def reader(index):
        return lambda: core_clock_text(index)

    return [
        {
            "name": label,
            "value": reader(index),
            "Category": category,
            "Tab": SENSOR_TAB,
            "Column": column,
            "read_type": "standard",
            "live": True,
            PARENT_ROW_KEY: CORE_EFFECTIVE_CLOCK_ROW,
        }
        for index, label in enumerate(labels)
    ]


# --- The card, live.
#
# The AM5 profile reads these through AMD's own libraries; here they come from
# NVML, which the graphics identity rows already use. Labels match the AM5
# side wherever the two report the same thing, so a row means one thing across
# both platforms rather than two names for one reading.
#
# Read once per tick rather than once per row: eight rows asking separately
# would open the same session eight times a second. The first row of the group
# refreshes the set and the rest read what it left, which is why the order
# below is load-bearing.
GRAPHICS_SENSOR_CACHE = {}


def _graphics_sensors(refresh=False):
    if refresh or not GRAPHICS_SENSOR_CACHE:
        try:
            from nvidia_gpu import read_gpu_sensors

            readings = read_gpu_sensors()
            GRAPHICS_SENSOR_CACHE.clear()
            GRAPHICS_SENSOR_CACHE.update(readings)
        except Exception:
            GRAPHICS_SENSOR_CACHE.clear()
    return GRAPHICS_SENSOR_CACHE


def _graphics_sensor(label, first):
    """One card reading. The first row of the group refreshes the set."""
    return _graphics_sensors(refresh=first).get(label)


GRAPHICS_SENSOR_LABELS = (
    "GPU Clock",
    "GPU Memory Clock",
    "GPU Video Clock",
    "GPU Core Voltage",
    "GPU Temperature",
    "GPU Fan PWM",
    "GPU Utilization",
    "GPU Board Power (TBP)",
    # Directly under the power it is heading for: a power figure means little
    # without the number it is measured against.
    "GPU Board Power Limit",
    "GPU Memory Used",
)

GRAPHICS_SENSOR_ROWS = tuple(
    (label, "Graphics",
     (lambda label=label, first=(index == 0):
      _graphics_sensor(label, first)),
     "Right")
    for index, label in enumerate(GRAPHICS_SENSOR_LABELS)
)


# --- The hottest core, from the die rather than from the board.
#
# CPU Temp above is the Super I/O's CPU channel: a board sensor, steady at
# 33 C here while the die spikes. This is the die's own reading, and it is
# what a tuner watches against TjMax.
#
# Located by sweeping the whole of MCHBAR across a controlled load rather
# than assumed. Idle -> 16-process load -> idle, twice, keeping only registers
# holding a plausible Celsius value that rose by at least 8 and came back:
#
#   0x2914, 0x2994, 0x8C14   31 idle, 62 loaded, flat and slow
#   0x5978, 0x597C           33 idle, 77 loaded, spiky and immediate
#
# The second group is the die. It jitters 33 -> 44 -> 34 at idle the way DTS
# does when a core briefly wakes, where the first group holds 31 without
# moving; and it returns to idle the moment load stops, which a sensor with
# more mass behind it does not. RAPL power, for contrast, lags a whole sample
# window on the way down, so this is not a power counter either.
#
# The two offsets mirror one value: 198 of 200 back-to-back reads at idle
# agreed exactly, and 60 of 60 under a single-core load, where two separate
# sensors would have parted. So this is one reading available twice, not the
# package and the hottest core separately -- which is also why there is one
# row here and not two. On Intel the package sensor is the hottest core in
# any case, which is why HWiNFO's CPU Package and Core Max track each other.
#
# What is NOT here: per-core temperatures. The sweep covered all 131072 bytes
# of MCHBAR and found five temperature-shaped registers, not the twenty-four
# a 14900KS would need. Per-core DTS is in IA32_THERM_STATUS, one MSR per
# logical processor, and this driver has no RDMSR.
CORE_MAX_TEMP_OFFSET = 0x5978
CORE_MAX_TEMP_MIRROR = 0x597C

# Nothing outside this is a temperature. Zero is the register unread rather
# than a die at freezing, and TjMax is 100 C on this part.
CORE_MAX_TEMP_RANGE = (5, 125)


def get_core_max_temp():
    """The hottest core in degrees C, or None."""
    try:
        raw = read_physical_memory_int(MCHBAR + CORE_MAX_TEMP_OFFSET, 4)
        if raw is None:
            return None
        raw = int(raw)
        low, high = CORE_MAX_TEMP_RANGE
        if not low <= raw <= high:
            return None
        return "%.1f °C" % raw
    except Exception:
        return None


def _whea_errors():
    """How many hardware errors Windows has logged since this boot."""
    try:
        from whea_errors import error_text

        return error_text()
    except Exception:
        return None


# The one counter that belongs beside the readings rather than in them. A kit
# that boots and benchmarks can still be quietly correcting errors, and this
# is the only place in the tool that would say so.
#
# It reads the Windows event log, which has nothing to do with whose memory
# controller this is. It was on the AM5 profile alone only because that is
# where it happened to be written.
#
# It sits in the window that keeps a maximum, which is the shape that matters
# here: a count that ticked up once during a run is the whole point, and a tab
# showing the current value would lose it.
ERROR_SENSOR_ROWS = (
    ("WHEA Errors", "Errors", _whea_errors, "Right"),
)


SENSOR_ROWS = (
    # The clocks, which a tab can only ever show as an instant. What BCLK does
    # across a stability run, and how far the ring falls back under load, are
    # exactly the readings worth having a minimum and a maximum for.
    #
    # Bus and ring come out of MCHBAR and agree with HWiNFO exactly -- 100.0
    # MHz and 5000.0 MHz on this bench. The memory clocks follow them because
    # this is a memory tool and they are the reason the window is open.
    #
    # The core clocks are a different measurement and are named for it. There
    # is no RDMSR here, so the P-state ratio HWiNFO reads is out of reach;
    # these come from the Windows performance counters, which average across
    # the sampling interval. See cpu_clocks for the two routes measured and
    # rejected before that one.
    ("Bus Clock", "Clocks", get_bclk_rd, "Left"),
    ("Core Clock (avg)", "Clocks", lambda: _core_clock("core_avg"), "Left"),
    ("Core Effective Clock", "Clocks",
     lambda: _core_clock("core_effective"), "Left"),
    ("Ring/LLC Clock", "Clocks", get_ring_freq, "Left"),
    ("MCLK", "Clocks", get_mclk, "Left"),
    ("UCLK", "Clocks", get_uclk, "Left"),
    ("DRAM Frequency", "Clocks", get_dram_frequency, "Left"),
    # Temperatures and power share one section, the way the AM5 profile's
    # THERMAL & POWER block does, and read in that order: what the parts are
    # sitting at, then what the package is drawing to get there. They were two
    # groups only because the Intel profile grew the power rows last.
    #
    # One row per DIMM channel: the two sit in different airflow, and the
    # hotter one is what matters when tuning. Each carries its own peak.
    ("DIMM A Temp", "Thermal & Power", lambda: _dimm_temperature("a"), "Right"),
    ("DIMM B Temp", "Thermal & Power", lambda: _dimm_temperature("b"), "Right"),
    # The VRM leads the board sensors: it is what delivers the DRAM rail, so
    # it is the one that matters while that rail is being pushed.
    ("VRM Temp", "Thermal & Power", lambda: _board_temperature("vrm"), "Right"),
    ("CPU Temp", "Thermal & Power", lambda: _board_temperature("cpu"), "Right"),
    # Directly under the board's CPU channel, because the two are the same
    # subject measured in different places and the difference is the point.
    ("Core Max", "Thermal & Power", get_core_max_temp, "Right"),
    ("CPU Socket Temp", "Thermal & Power",
     lambda: _board_temperature("socket"), "Right"),
    ("PCH Temp", "Thermal & Power", lambda: _board_temperature("pch"), "Right"),
    ("System Temp", "Thermal & Power",
     lambda: _board_temperature("system"), "Right"),
    ("CPU Package Power", "Thermal & Power",
     lambda: _package_power("package"), "Left"),
    ("CPU Cores Power", "Thermal & Power",
     lambda: _package_power("cores"), "Left"),
    # VCCSA and CPU SA are the same rail measured in two places, the memory
    # controller and the board VRM, so they sit adjacent for comparison.
    ("DLVR Vcore", "Voltages", lambda: _board_rail("vcore"), "Left"),
    ("VCCSA", "Voltages", get_sa, "Left"),
    ("CPU SA (VRM)", "Voltages", lambda: _board_rail("cpu_sa"), "Left"),
    ("VDDQ TX", "Voltages", get_tx, "Left"),
    # VDD2 is the memory controller's own supply and the rail a DDR5 tuner
    # reaches for first, so it leads the memory side of the list rather than
    # trailing the auxiliary CPU rails.
    ("VDD2", "Voltages", lambda: _board_rail("vdd2"), "Left"),
    ("VTT", "Voltages", lambda: _board_rail("vtt"), "Left"),
    ("VCCIO", "Voltages", lambda: _board_rail("vccio"), "Left"),
    ("CPU VNNAON", "Voltages", lambda: _board_rail("vnnaon"), "Left"),
    ("CPU AUX", "Voltages", lambda: _board_rail("cpu_aux"), "Left"),
    # DRAM against DRAM VDD is the same rail measured in two places, like
    # VCCSA against CPU SA above: the board's own sense point, then the
    # module's PMIC. A board that reports only one of them leaves the other
    # blank rather than borrowing.
    ("DRAM", "Voltages", lambda: _board_rail("vdimm"), "Left"),
    ("DRAM VDD", "Voltages", lambda: _dram_rail("dram_vdd"), "Left"),
    ("DRAM VDDQ", "Voltages", lambda: _dram_rail("dram_vddq"), "Left"),
    ("DRAM VPP", "Voltages", lambda: _dram_rail("dram_vpp"), "Left"),
) + GRAPHICS_SENSOR_ROWS + ERROR_SENSOR_ROWS

# Rows the sensor rows replace, wherever the earlier passes left them.
SUPERSEDED_SENSOR_ROWS = {
    ("VCCSA", "System Info"),
    ("VDDQ TX", "System Info"),
    ("SA Voltage", "CPU"),
    ("TX Voltage", "CPU"),
}


# Rows for hardware LGA 1851 does not have, dropped rather than left blank.
#
# A row is listed here only when the reading does not exist on this platform,
# never when it exists but is not implemented yet. DRAM VDD/VDDQ/VPP and the
# DIMM temperatures stay on the tab for exactly that reason: the modules do
# report them, they are simply not reachable until the DDR5 SPD transport is
# written. A blank there means "not read yet"; a row here would mean "not
# there at all", and the two must not look the same.
#
#   VDDQ TX          not a rail on this socket.
#   VCCSA            MCHBAR 0x591C fed it on earlier sockets and reads 0 here;
#                    "CPU SA (VRM)" is the live measurement of the same rail.
#   CPU AUX          not a rail on this socket.
#   CPU Socket Temp  neither ITE chip carries a socket channel: IT8696E
#                    reports System1/PCH/CPU/PCIEX16/VRM MOS and IT87952E
#                    reports PCIEX4/System2.
#   DRAM             the board has no VDIMM sense channel; the module's own
#                    PMIC rail in DRAM VDD is the measurement that exists.
ARROW_LAKE_ABSENT_SENSOR_ROWS = (
    "VDDQ TX",
    "VCCSA",
    "CPU AUX",
    "CPU Socket Temp",
    "DRAM",
)


# Rows an LGA 1700 DDR5 board has no reading for. Same rule as the list above:
# listed only where the reading does not reach this platform at all.
#
#   VCCIO       not on the Super I/O this project drives. HWiNFO reports an
#               "IVR VCCIO Analog" on the Z790 bench, but through the ASUS EC
#               -- a transport nothing here speaks. The rail is real and
#               unreachable, which is why it is dropped rather than blank.
#   CPU VNNAON  an LGA 1851 rail. Nothing on the Z790 bench reports one, on
#               the Super I/O or anywhere else.
#   DRAM        the NCT6798D carries no VDIMM sense channel. The module's own
#               PMIC is the only VDD measurement this board has.
LGA1700_DDR5_ABSENT_SENSOR_ROWS = (
    "VCCIO",
    "CPU VNNAON",
    "DRAM",
)

# Rows that read correctly here and still come off the tab, which is a
# different thing from the list above and is kept separate so the two are
# never read as the same statement.
#
# All of these are reported per module in the Telemetry window instead, each
# with its own minimum, maximum and average:
#
#   DRAM VDD/VDDQ/VPP  one aggregated row has to pick a module to believe, and
#                      cannot show that the two sticks differ -- which they do
#                      here, by 15 mV on VPP.
#   DIMM A/B Temp      the same reading, under the panel of the module it
#                      belongs to. The tab pair named a channel; the panels
#                      name the slot, the part and the PMIC alongside it.
LGA1700_DDR5_PER_DIMM_SENSOR_ROWS = (
    "DRAM VDD",
    "DRAM VDDQ",
    "DRAM VPP",
    "DIMM A Temp",
    "DIMM B Temp",
)

# Rows an LGA 1700 DDR4 board has no reading for. Two kinds, both absences:
#
#   VDD2        the DDR5 memory supply. DDR4 has no such rail -- the module
#               runs off VDDQ, which the DRAM row already carries.
#   CPU VNNAON  an LGA 1851 rail, as on the DDR5 list. Raptor Lake has none.
#   VTT         DDR4 terminates the command bus at half VDDQ, but that is
#               derived on the module. The separate VTT supply this row was
#               written for is a Skylake-era board rail, and no LGA 1700 VRM
#               or Super I/O channel reports one.
#   VCCIO       folded into VCCSA and VDDQ TX on this socket, and unreachable
#               for the same reason it is on the DDR5 list.
#
# The three PMIC rails are the second kind. A DDR4 module has no PMIC at all
# -- its rails are supplied by the board -- so there is nothing to read, and
# what the code did instead is why DRAM VPP is on this list despite having
# printed a value. read_dram_rails sweeps 0x48-0x4F for a PMIC; on this bench
# 0x4F answers, it is not a PMIC, and its registers decoded to a VPP of
# 1.500 V. DDR4 VPP is 2.5 V, so the number was not merely from the wrong
# device, it was not a voltage this rail can hold. See _dram_rail, which now
# declines the sweep on DDR4 rather than relying on the row being absent.
#
# VCCSA is on the list for neither reason. It reads correctly here -- 1.201 V
# against the VRM's 1.200 V -- and comes off because that is the same rail
# twice, which is exactly why it comes off the DDR5 board. See
# LGA1700_DDR5_DUPLICATE_SENSOR_ROWS; what remains is the VRM-side figure,
# under the rail's own name.
LGA1700_DDR4_ABSENT_SENSOR_ROWS = (
    "VDD2",
    "VTT",
    "VCCIO",
    "CPU VNNAON",
    "DRAM VDD",
    "DRAM VDDQ",
    "DRAM VPP",
    "VCCSA",
)

# Rows dropped because the row beside them already carries the reading.
#
#   VCCSA  the system agent rail measured on-die through MCHBAR. "SA" below is
#          the same rail measured at the board VRM, and the two sat adjacent
#          precisely to be compared. Keeping one means the tab reports the
#          rail once; they differ by about 20 mV here, and what remains is the
#          VRM-side figure.
LGA1700_DDR5_DUPLICATE_SENSOR_ROWS = (
    "VCCSA",
)


# Row labels that differ by platform: {platform: {declared name: label}}.
#
# A rename, not a different row -- the rail, the register and the reading are
# the same either way, and only the name a tuner is looking for changes.
#
# The row is declared "DLVR Vcore" because on LGA 1851 the Super I/O channel
# is the DLVR output, and naming that stage is the point there. On LGA 1700 the
# same channel is the VRM's core rail: HWiNFO calls it plain "Vcore" reading
# this chip, so carrying "DLVR" here would name a stage the reading does not
# come from.
# "CPU SA (VRM)" said which of two measurement points it was, because VCCSA
# sat beside it reading the same rail on-die. With VCCSA gone from this
# platform the qualifier distinguishes it from nothing, so the row takes the
# rail's own name.
#
# Both LGA 1700 memory variants take the same two names. They are one socket
# reading one Super I/O; which DRAM generation is fitted has nothing to do
# with what the core and system-agent channels are called, and listing only
# the DDR5 half left the DDR4 board saying "DLVR" for a stage it does not have.
LGA1700_SENSOR_ROW_LABELS = {
    "DLVR Vcore": "Vcore",
    "CPU SA (VRM)": "SA",
}

SENSOR_ROW_LABELS = {
    LGA1700_DDR4: LGA1700_SENSOR_ROW_LABELS,
    LGA1700_DDR5: LGA1700_SENSOR_ROW_LABELS,
}


def sensor_row_label(platform, name):
    """The label a row carries on this platform. Pure, so it can be tested."""
    return SENSOR_ROW_LABELS.get(platform, {}).get(name, name)


# Rows a particular board does not wire, whatever its socket supports. Kept
# apart from the platform lists because the reason is different: those rows
# are absent from the silicon, these are present in it and not brought out.
#
#   CPU Socket Temp  the NCT6798D on the ASUS Z790 APEX has no socket channel.
#                    It reports Motherboard, CPU (weighted), CPU Package, CPU
#                    and PCH -- the same five HWiNFO lists against that chip
#                    -- and none of them is the socket. The row read blank on
#                    every tick, which in this tool means "not read yet" and
#                    said the wrong thing about a channel that is not there.
ASUS_ABSENT_SENSOR_ROWS = ("CPU Socket Temp",)

BOARD_ABSENT_SENSOR_ROWS = (
    ("ASUS", ASUS_ABSENT_SENSOR_ROWS),
)


def board_absent_sensor_rows(manufacturer):
    """Rows the named board maker does not wire. Pure, so it can be tested
    without being on that board.

    Matched on a prefix of the upper-cased name: SMBIOS spells the same maker
    "ASUSTeK COMPUTER INC." here and "ASUS" elsewhere.
    """
    name = (manufacturer or "").upper()
    for prefix, rows in BOARD_ABSENT_SENSOR_ROWS:
        if name.startswith(prefix):
            return rows
    return ()


def absent_sensor_rows(platform, arrow_lake, manufacturer=None):
    """Return the sensor rows this platform does not install.

    Pure, so which rows a platform drops can be checked without importing the
    table and without being on that platform.
    """
    board = board_absent_sensor_rows(manufacturer)
    if arrow_lake:
        return ARROW_LAKE_ABSENT_SENSOR_ROWS + board
    if platform == LGA1700_DDR5:
        return (
            LGA1700_DDR5_ABSENT_SENSOR_ROWS
            + LGA1700_DDR5_PER_DIMM_SENSOR_ROWS
            + LGA1700_DDR5_DUPLICATE_SENSOR_ROWS
            + board
        )
    if platform == LGA1700_DDR4:
        return LGA1700_DDR4_ABSENT_SENSOR_ROWS + board
    return board


def _install_sensors_tab():
    global TIMINGS

    TIMINGS = [
        timing for timing in TIMINGS
        if (timing.get("name"), timing.get("Tab")) not in SUPERSEDED_SENSOR_ROWS
    ]

    platform = active_platform()
    absent = absent_sensor_rows(platform, is_arrow_lake_platform(),
                                get_board_manufacturer())

    for name, category, getter, column in SENSOR_ROWS:
        if name in absent:
            continue
        TIMINGS.append({
            "name": sensor_row_label(platform, name),
            "value": getter,
            "Category": category,
            "Tab": SENSOR_TAB,
            "Column": column,
            "read_type": "standard",
            "live": True,
        })
        if name == CORE_EFFECTIVE_CLOCK_ROW:
            TIMINGS.extend(_per_core_clock_rows(category, column))


_install_sensors_tab()


# --- Refresh mode leads the refresh timings.
#
# The row used to sit on the Skew tab, a section of one, while the timings it
# governs were a tab away. It now heads the Refresh timings section, because
# the mode is what tREFI and tREFIx9 are read under.
#
# It is the one row in that section without channel columns: on DDR4 it is
# decoded from DDR_PTM_CTL at MCHBAR + 0x5880, in the global register region
# rather than the per-controller block, so there is no second-channel copy of
# it to show. The section renders it as a value under A1 with B1 blank, which
# is only tolerable because REFRESH_MODE_LABELS keeps the text as narrow as the
# timings beside it - see the note there.
#
# Both platform paths survive the move untouched: DDR4 arrives as a decoded
# value, DDR5 as the dynamic mode-register read declared in the table.
REFRESH_MODE_SOURCE = ("REFRESH", "REFRESH MODE")
REFRESH_MODE_NAME = "Refresh Mode"
REFRESH_TIMINGS_CATEGORY = "Refresh timings"


def _install_refresh_mode_row():
    row = None
    for timing in list(TIMINGS):
        if (timing.get("name"), timing.get("Category")) == REFRESH_MODE_SOURCE:
            row = timing
            TIMINGS.remove(timing)
            break
    if row is None:
        return

    row["name"] = REFRESH_MODE_NAME
    row["Category"] = REFRESH_TIMINGS_CATEGORY
    row["Tab"] = "Timings"

    # First row of the section it now belongs to.
    for index, timing in enumerate(TIMINGS):
        if (
            timing.get("Tab") == "Timings"
            and timing.get("Category") == REFRESH_TIMINGS_CATEGORY
        ):
            row["Column"] = timing.get("Column", "Left")
            TIMINGS.insert(index, row)
            return
    TIMINGS.append(row)


_install_refresh_mode_row()


# --- Rows the active refresh mode is not using.
#
# tRFCpb is the per-bank refresh interval. Under the normal all-bank tRFC
# schedule the controller does not refresh a bank at a time, so the register
# still reads but nothing acts on it, and showing it at full weight implies
# otherwise. main.py dims a row whose "dim" callable returns True, which is
# what that hook exists for.
#
# Attached here rather than on the row literal because refresh_is_normal is
# defined further down the file: the literal would capture the name before it
# is bound.
REFRESH_MODE_DIMMED_ROWS = ("tRFCpb",)


def _install_refresh_mode_dimming():
    for timing in TIMINGS:
        if timing.get("name") in REFRESH_MODE_DIMMED_ROWS:
            timing["dim"] = refresh_is_normal


_install_refresh_mode_dimming()


# --- Timings tab column balance.
#
# Sections were assigned to columns row by row as the table grew, and the split
# had drifted to 43 lines on the left against 30 on the right: the left column
# ran past the bottom of the window and scrolled while the right half of the
# tab sat empty.
#
# Assigning whole sections here rather than per row keeps a section in one
# piece and puts the arithmetic in one place where it can be checked. The split
# below is 36 lines against 37, counting section headings.
#
# Left keeps the sequence a tuner reads top to bottom: the primaries, the
# secondaries, then how refresh is configured. Right takes the two long
# reference blocks.
TIMINGS_TAB_COLUMNS = {
    "Primary": "Left",
    "Secondary": "Left",
    "Other Timings": "Left",
    "Command": "Left",
    "Refresh timings": "Right",
    "Tertiary": "Right",
    "Power down": "Right",
}

# Power down had become the tab's catch-all: 25 rows holding the actual
# power-down entry and exit timings alongside chip-select spacing, mode
# register command timings, preamble and precharge, refresh management and
# the DLL lock time. Each of those belongs with what it describes.
#
# Applied by name after the rows are built, rather than by editing each
# definition, so the grouping is legible in one place and a row that moves
# again does not need its declaration hunted down.
TIMINGS_SECTION_MOVES = {
    # Refresh management and the refresh interval read with the intervals.
    "tRFM": "Refresh timings",
    "OREF_RI": "Refresh timings",
    # Precharge, preamble and the DLL lock are ordinary bus timings.
    "tRDPRE": "Other Timings",
    "tWRPRE": "Other Timings",
    "tWPRE": "Other Timings",
    "tDLLK": "Other Timings",
    # Chip select spacing and the mode-register command timings.
    "tCSH": "Command",
    "tCSL": "Command",
    "tCA2CS": "Command",
    "tOSCO": "Command",
    "tPREMRR": "Command",
    "tMRR": "Command",
    # The mode-register write timing sits with the read it pairs with.
    "tMRRMRW": "Command",
}


def _install_timings_sections():
    """Move rows out of Power down into the section that describes them."""
    for timing in TIMINGS:
        if timing.get("Tab") != "Timings":
            continue
        category = TIMINGS_SECTION_MOVES.get(timing.get("name"))
        if category is not None:
            timing["Category"] = category


def _group_timings_sections():
    """Keep each section's rows together so it draws as one block.

    The tab is built by walking its rows in order and starting a new block
    every time the category changes, so a category whose rows are not
    contiguous renders as two headings with the same name -- Refresh timings
    appeared three times, and Power down and Other Timings twice each, after
    the pass above re-categorised rows without moving them.

    Categories keep the order they first appear in and rows keep their order
    within a category; only the gaps close. Section order on the page is
    decided later, by TIMINGS_SECTION_ORDER.
    """
    positions = [index for index, timing in enumerate(TIMINGS)
                 if timing.get("Tab") == "Timings"]
    if not positions:
        return

    order, grouped = [], {}
    for index in positions:
        category = TIMINGS[index].get("Category")
        if category not in grouped:
            grouped[category] = []
            order.append(category)
        grouped[category].append(TIMINGS[index])

    regrouped = [row for category in order for row in grouped[category]]
    for index, row in zip(positions, regrouped):
        TIMINGS[index] = row


_install_timings_sections()
_group_timings_sections()


def _install_timings_column_layout():
    for timing in TIMINGS:
        if timing.get("Tab") != "Timings":
            continue
        column = TIMINGS_TAB_COLUMNS.get(timing.get("Category"))
        if column is not None:
            timing["Column"] = column


_install_timings_column_layout()


# --- System Info identity and per-module rows.
#
# The tab already described the controller and its settings but not the machine
# around it, and it named the kit only by its module vendor. These rows close
# both gaps, in the order the AM5 System Info uses: what the machine is, then
# the board, then what is actually installed in it.

SYSTEM_INFO_TAB = "System Info"

PLATFORM_LABELS = {
    LGA1700_DDR4: "LGA1700 (DDR4)",
    LGA1700_DDR5: "LGA1700 (DDR5)",
    LGA1851: "LGA1851 (DDR5)",
}


WINDOWS_VERSION_KEY = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion"


def _windows_version_value(name):
    """One value from Windows' own version key, or None."""
    return _windows_version_values().get(name)


@lru_cache(maxsize=None)
def _windows_version_values():
    """The version key's values, read in one open.

    Cached, and read together: these cannot change while the machine runs,
    and the row that wants them is re-read on every Advanced window tick.
    """
    found = {}
    try:
        with winreg.OpenKey(
            winreg.HKEY_LOCAL_MACHINE, WINDOWS_VERSION_KEY
        ) as key:
            for name in ("UBR", "DisplayVersion", "ReleaseId"):
                try:
                    found[name], _kind = winreg.QueryValueEx(key, name)
                except OSError:
                    continue
    except Exception:
        return {}
    return found


def get_windows_update_revision():
    """The UBR: the fourth part of the build number.

    WMI stops at the build, so 22631 is as much as Win32_OperatingSystem will
    say. The revision that follows it is what moves with each cumulative
    update, and it lives in the registry.
    """
    value = _windows_version_value("UBR")
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def get_windows_display_version():
    """The feature update -- 23H2, 24H2 -- as winver names it.

    DisplayVersion is the one to read. ReleaseId looks like it would do and
    does not: Microsoft froze it at "2009" when the naming changed, so on this
    bench it reads 2009 beside a DisplayVersion of 23H2. It is consulted only
    where DisplayVersion does not exist, which is the era when it was still
    the accurate one.
    """
    version = _windows_version_value("DisplayVersion")
    if version is None:
        version = _windows_version_value("ReleaseId")
    version = str(version or "").strip()
    return version or None


@_identity_cached
def get_os_name():
    """Windows edition, architecture and full build, as CPU-Z states it.

    Its About tab reads "Microsoft Windows 11  Professional (x64) Build
    22631.6199", which is the same facts WMI carries plus the update revision
    -- so the parts are assembled here rather than the caption being printed
    as-is. "Pro" is spelled the way the edition is spelled: WMI abbreviates it
    in the caption while the edition itself is Professional.
    """
    try:
        systems = _wmi_static("Win32_OperatingSystem")
        if not systems:
            return None
        caption = (getattr(systems[0], "Caption", "") or "").strip()
        if not caption:
            return None
        if not caption.startswith("Microsoft "):
            caption = "Microsoft " + caption
        for short, full in (("Pro", "Professional"), ("Ent", "Enterprise")):
            if caption.endswith(" " + short):
                caption = caption[: -len(short)] + full
        architecture = (
            getattr(systems[0], "OSArchitecture", "") or ""
        ).strip()
        if architecture:
            # WMI says "64-bit"; CPU-Z, and everyone else, says x64.
            caption += " (x64)" if "64" in architecture else " (x86)"
        # winver's ordering: the feature update, then the build it produced.
        display_version = get_windows_display_version()
        if display_version:
            caption += " " + display_version
        build = (getattr(systems[0], "BuildNumber", "") or "").strip()
        if build:
            revision = get_windows_update_revision()
            caption += " Build %s" % build
            if revision is not None:
                caption += ".%d" % revision
        return caption
    except Exception as e:
        print(f"Error retrieving OS name: {e}")
        return None


def get_gpu_driver_date():
    """Release date of the installed display driver.

    The version alone does not say how old a driver is. Same WMI datetime
    shape as the BIOS release date, and rendered the same way.
    """
    try:
        for adapter in _real_display_adapters():
            date = _wmi_date(getattr(adapter, "DriverDate", ""))
            if date:
                return date
        return None
    except Exception as e:
        print(f"Error retrieving GPU driver date: {e}")
        return None


def get_gpu_name():
    """Every real display adapter.

    The Microsoft Basic Display driver is skipped: Windows installs it before
    the vendor driver and it is not the card in the machine.
    """
    try:
        names = []
        for adapter in _wmi_static("Win32_VideoController"):
            name = (getattr(adapter, "Name", "") or "").strip()
            if name and "Basic Display" not in name and name not in names:
                names.append(name)
        return " / ".join(names) if names else None
    except Exception as e:
        print(f"Error retrieving GPU name: {e}")
        return None


def get_bios_date():
    """Release date of the installed BIOS.

    The version string alone does not say how old a build is, and on this board
    the vendor reuses short version numbers across releases.
    """
    try:
        entries = _wmi_static("Win32_BIOS")
        if not entries:
            return None
        return _wmi_date(getattr(entries[0], "ReleaseDate", ""))
    except Exception as e:
        print(f"Error retrieving BIOS date: {e}")
        return None


def get_slots_used():
    """Populated sockets against the board's total, e.g. "2 of 4".

    Two modules in a four-slot board behave differently from four, so the
    total matters as much as the count.
    """
    try:
        arrays = _wmi_static("Win32_PhysicalMemoryArray")
        if not arrays:
            return None
        total = int(getattr(arrays[0], "MemoryDevices", 0) or 0)
        used = len(_wmi_static("Win32_PhysicalMemory"))
        if not total:
            return None
        return "%d of %d" % (used, total)
    except Exception as e:
        print(f"Error retrieving slot usage: {e}")
        return None


def get_platform_name():
    """Socket and memory generation, named by the classification that chose
    this backend rather than by a second guess at the same facts.

    Through active_platform for that reason, and for a second one: this row
    is read on every refresh, and detect_current_platform opens a WMI
    connection and runs three queries -- 1.08 seconds on the bench, measured.
    Calling it here put that second into each pass over the table.
    """
    try:
        return PLATFORM_LABELS.get(active_platform())
    except Exception as e:
        print(f"Error retrieving platform: {e}")
        return None


def _ic_lookup(module, field):
    """The DRAM maker or die inferred from the module's part number."""
    from dimm_inventory import split_ic

    return split_ic(module["ic"])[0 if field == "dram_manufacturer" else 1]


def _named_or_nothing(module, field):
    """A firmware string, unless firmware left it as a placeholder."""
    value = str(module.get(field) or "").strip()
    return None if value.lower() in ("", "unknown") else value


# Fields the module's own SPD answers, and what stands in when the bus cannot
# be reached -- no driver, no admin, or DDR4, whose identity block this
# transport cannot get at. A None fallback means nothing else on the machine
# carries the field, so the row reports nothing rather than inventing it:
# SMBIOS has no build date at all, and the maker/die lookup is keyed on the
# part number and so cannot know an unlisted kit.
#
# The serial is the exception among those three: firmware does read it off the
# module at POST and puts it in Type 17, so it comes back through the same
# placeholder filter as the rest.
SPD_IDENTITY_FIELDS = {
    "module_manufacturer": _named_or_nothing,
    "part_number": _named_or_nothing,
    "serial_number": _named_or_nothing,
    "manufacture_date": None,
    "dram_manufacturer": _ic_lookup,
    "dram_die": _ic_lookup,
}

# What the DDR4 SPD is asked for. Only the two fields nothing else on the
# machine carries: SMBIOS has no build date at all, and reports a serial only
# when firmware found one.
#
# The rest are deliberately left on the path that already answers them. The
# DDR4 block does carry a DRAM maker and a stepping, but the stepping is the
# byte the die name would come from and it reads 0x00 on this kit -- taking
# SPD first there would turn a "B-die" that the part-number table gets right
# into a "0x00" that is technically what the module said and no use to anyone.
DDR4_SPD_FIELDS = ("serial_number", "manufacture_date")


def _spd_identity(field):
    """What the modules themselves say about one field, or None.

    Two readers, picked by generation: the SPD layouts share no offsets and
    the DDR5 one reaches its block through a page-select write that a DDR4
    module would take as a write to its own SPD array. See ddr4_spd.
    """
    from dimm_inventory import EM_DASH, shared_value

    if detect_ddr_generation() == "DDR4":
        if field not in DDR4_SPD_FIELDS:
            return None
        from ddr4_spd import read_identity
    else:
        from ddr5_spd import read_identity

    modules = read_identity()
    if not modules:
        return None
    value = shared_value(modules, lambda entry: entry.get(field))
    return None if value == EM_DASH else value


def _dimm_field(field):
    """Return a per-DIMM System Info value for the whole installed set.

    Size and rank come from SMBIOS Type 17, which is firmware reporting what it
    read from the modules at boot.

    The DRAM maker, the die and the build date come from the module's own SPD,
    read over the PCH SMBus -- the same bytes CPU-Z shows on its SPD tab.
    SMBIOS carries none of the three. See _spd_identity for which reader
    answers on which generation, and DDR4_SPD_FIELDS for why DDR4 is asked a
    narrower question. If the bus is unreachable -- no driver, no admin -- the
    maker and die fall back to a lookup from the part number, and the date,
    which no table carries, reports nothing.

    Reported for the set rather than one slot, since the controller settings
    above these rows are equally set-wide. A mixed kit prints each distinct
    value instead of hiding the mismatch.
    """
    try:
        from dimm_inventory import (
            EM_DASH, rank_numeric, read_modules, shared_value,
        )

        if field in SPD_IDENTITY_FIELDS:
            # The module first, firmware second. SMBIOS carries no date at
            # all, reports a serial only when firmware found one, and where it
            # does carry a name it is the string a board vendor typed:
            # "V-Color Technology Inc" against the "V-Color Technology" the
            # module itself reports.
            value = _spd_identity(field)
            if value is not None:
                return value
            fallback = SPD_IDENTITY_FIELDS[field]
            if fallback is None:
                return None
            value = shared_value(
                read_modules(), lambda module: fallback(module, field)
            )
            return None if value == EM_DASH else value

        modules = read_modules()
        if field == "size":
            value = shared_value(
                modules,
                lambda module: (
                    "%d GB" % module["capacity_gb"]
                    if module["capacity_gb"] else EM_DASH
                ),
            )
        elif field == "rank":
            value = shared_value(
                modules, lambda module: rank_numeric(module["rank_count"])
            )
        else:
            return None
        # Hand the em dash back as "no value" so every unavailable row in the
        # table renders the same way.
        return None if value == EM_DASH else value
    except Exception:
        return None


def _system_info_row(name, getter):
    return {
        "name": name,
        "value": getter,
        "Category": "General",
        "Tab": SYSTEM_INFO_TAB,
        "Column": "Left",
        "read_type": "standard",
    }


def _place_system_info_rows(anchor, rows, after=True):
    """Insert rows beside an existing System Info row.

    Position is taken from a named neighbour rather than an index, so an
    earlier pass reordering the tab carries these rows with it. If the
    neighbour is gone the rows still appear, at the end.
    """
    for index, timing in enumerate(TIMINGS):
        if (
            timing.get("name") == anchor
            and timing.get("Tab") == SYSTEM_INFO_TAB
        ):
            at = index + 1 if after else index
            TIMINGS[at:at] = rows
            return True
    TIMINGS.extend(rows)
    return False


def _install_system_info_identity_rows():
    # What the machine is, above the CPU it is built on.
    _place_system_info_rows(
        "CPU",
        [
            _system_info_row("Platform", get_platform_name),
            _system_info_row("OS", get_os_name),
        ],
        after=False,
    )
    _place_system_info_rows(
        "Cores / Threads",
        [_system_info_row("GPU", get_gpu_name)],
    )
    # What the silicon is, beside the name it is sold under.
    _place_system_info_rows("CPU", [
        _system_info_row("Code Name", get_cpu_codename),
        _system_info_row("Technology", get_cpu_technology),
    ])
    # The board's own silicon, under the board it sits on.
    _place_system_info_rows("BIOS Date", [
        _system_info_row("Chipset", get_chipset_name),
        _system_info_row("Southbridge", get_southbridge_name),
        _system_info_row("LPCIO", get_lpcio_name),
    ])
    _place_system_info_rows("Channels", [
        _system_info_row("Type", get_memory_type),
    ], after=False)
    _place_system_info_rows("GPU", [
        _system_info_row(label, partial(_gpu_field, field))
        for label, field in GPU_ROWS
    ] + [_system_info_row("Driver Date", get_gpu_driver_date)])
    # Beside the firmware version it dates, and the socket count beside the
    # modules filling them.
    _place_system_info_rows("BIOS", [
        _system_info_row("BIOS Date", get_bios_date),
    ])
    _place_system_info_rows("Memory Capacity", [
        _system_info_row("Slots Used", get_slots_used),
    ])
    # What is installed, next to the total it adds up to.
    _place_system_info_rows(
        "Memory Capacity",
        [
            _system_info_row("DIMM Size", lambda: _dimm_field("size")),
            _system_info_row("Rank", lambda: _dimm_field("rank")),
            _system_info_row(
                "DRAM Manufacturer", lambda: _dimm_field("dram_manufacturer")
            ),
            _system_info_row("DRAM Die", lambda: _dimm_field("dram_die")),
            # Ordered as CPU-Z's SPD tab reads them: what the module is, then
            # which one it is, then when it was built.
            _system_info_row(
                "Part Number", lambda: _dimm_field("part_number")
            ),
            _system_info_row(
                "Serial Number", lambda: _dimm_field("serial_number")
            ),
            _system_info_row(
                "Manufactured", lambda: _dimm_field("manufacture_date")
            ),
        ],
    )


_install_system_info_identity_rows()


# Controller policy bits rather than anything read or tuned from this tab.
#
# Power Down Mode is here because the Misc tab reads the same thing closer to
# the source: this row reported who owns the policy, decoded from DDR_PTM_CTL
# bit 6, while Misc reads the controller's own power-down bit. Summary now
# places that Misc row instead, so nothing is lost by dropping this one.
SYSTEM_INFO_REMOVED = (
    "CMD Stretch",
    "ECC / Error Correction",
    "Self Refresh",
    "Memory Scrambler",
    "Row Hammer",
    "Power Down Mode",
)


# The tab was 26 rows in one column under a single General heading, which is
# a long unbroken list to find anything in. Split into what the machine is,
# what board it runs on, the clock chain, and what memory is installed --
# each row keeping its place in SYSTEM_INFO_ORDER within its section.
#
# One column, the full width of the tab. That is not a leftover: the board
# row carries "ASUSTeK COMPUTER INC. ROG MAXIMUS Z790 APEX (Rev 1.xx)", which
# wants about 457px against the name beside it -- more than half a 700px
# window -- so a two-column split clips it. The column entry is kept so the
# arrangement is stated rather than implied, and so a future split has one
# place to change.
SYSTEM_INFO_SECTIONS = (
    ("System", "Left", ("OS", "Platform")),
    ("Processor", "Left", ("CPU", "Code Name", "Technology",
                           "Cores / Threads", "Microcode")),
    ("Motherboard", "Left", ("Manufacturer", "Model", "BIOS", "BIOS Date",
                             "Chipset", "Southbridge", "LPCIO")),
    # The memory speed first, then the ratios that set it, then the
    # clocks themselves from the reference outwards.
    ("Clocks", "Left", ("DRAM Frequency", "DRAM Ratio", "DDR QCLK Ratio",
                        "BCLK", "Uncore", "MCLK", "UCLK", "PSF0 PLL",
                        "Gear Mode")),
    ("Memory", "Left", ("Type", "Channels", "RAM Manufacturer",
                        "Memory Capacity", "Slots Used", "DIMM Size", "Rank",
                        "DRAM Manufacturer", "DRAM Die", "Part Number",
                        "Serial Number", "Manufactured")),
    ("Graphics", "Left", ("GPU", "Board Manufacturer", "GPU Code Name",
                          "GPU Revision", "Cores", "ROPs / TMUs",
                          "GPU Technology", "Memory Size", "Memory Type",
                          "Memory Vendor", "Bus Width", "Resizable BAR",
                          "Driver Version", "Driver Date")),
)


# --- System Info reading order.
#
# The rows had accumulated in whatever order the table happened to build them,
# interleaving identity, clocks and module facts. They now read in groups: what
# the machine is, the board, the clock chain from BCLK down to UCLK, how the
# controller is configured, then what is installed in it.
#
# Uncore and PSF0 PLL are not in the requested order but were not asked to be
# removed either, so they keep a place: Uncore among the clocks it belongs
# with, PSF0 PLL after the rest.
#
# Derived from the sections rather than listed again beside them. Held
# separately, every new row had to be added in two places and the two had
# already drifted -- Memory listed its rows in one order here and another
# there -- with a grouping pass quietly repairing the difference. One list
# cannot disagree with itself, and rows sorted by it are contiguous within a
# section by construction, so no grouping pass is needed.
SYSTEM_INFO_ORDER = tuple(
    name for _title, _column, names in SYSTEM_INFO_SECTIONS for name in names
)


def _install_system_info_sections():
    """Give each System Info row its section and column."""
    placement = {
        name: (title, column)
        for title, column, names in SYSTEM_INFO_SECTIONS
        for name in names
    }
    for timing in TIMINGS:
        if timing.get("Tab") != SYSTEM_INFO_TAB:
            continue
        section = placement.get(timing.get("name"))
        if section is None:
            # A row nobody placed keeps the old heading rather than vanishing,
            # which is how it stays visible long enough to be placed properly.
            continue
        timing["Category"], timing["Column"] = section


def _install_system_info_order():
    global TIMINGS

    TIMINGS = [
        timing for timing in TIMINGS
        if not (
            timing.get("Tab") == SYSTEM_INFO_TAB
            and timing.get("name") in SYSTEM_INFO_REMOVED
        )
    ]

    remaining = {}
    for timing in TIMINGS:
        if timing.get("Tab") == SYSTEM_INFO_TAB:
            remaining.setdefault(timing.get("name"), []).append(timing)
    if not remaining:
        return

    ordered = []
    for name in SYSTEM_INFO_ORDER:
        ordered.extend(remaining.pop(name, ()))
    # A row the order does not mention keeps a place at the end rather than
    # vanishing, so one added later shows up somewhere visible instead of
    # being dropped without trace.
    for leftover in remaining.values():
        ordered.extend(leftover)

    rebuilt = []
    placed = False
    for timing in TIMINGS:
        if timing.get("Tab") != SYSTEM_INFO_TAB:
            rebuilt.append(timing)
            continue
        if not placed:
            rebuilt.extend(ordered)
            placed = True
    TIMINGS = rebuilt


_install_system_info_order()
_install_system_info_sections()


# --- Misc tab.
#
# The CKE/power-down control block, the two SC_GS_CFG command fields and the
# controller feature switches. Sources are VoidTimings' register map, checked
# against live reads on the Z790 bench; the addresses below are the ones its
# map names, not guesses. That map has been wrong before (see the CLK Drv Dn
# note in the slew block), so anything not confirmed against a second source
# is flagged where it is decoded.
MISC_TAB = "Misc"
RTL_TAB = "RTL"

# One register holds all ten CKE fields. VoidTimings lists them in this order
# and shows them as plain numbers, so they are reported the same way rather
# than being decoded into cycles -- the units are not documented anywhere
# reachable, and a wrong unit is worse than a raw count.
MISC_CKE_CONFIG_OFFSET = 0xE0B8
MISC_CKE_CONFIG_FIELDS = (
    ("idle_length", 1, 4),
    ("powerdown_latency", 5, 5),
    ("powerdown_length", 10, 4),
    ("selfrefresh_latency", 14, 6),
    ("selfrefresh_length", 20, 4),
    ("ckevalid_length", 24, 4),
    ("ckevalid_enable", 28, 1),
    ("idle_enable", 29, 1),
    ("powerdown_enable", 30, 1),
    ("selfrefresh_enable", 31, 1),
)

# SC_GS_CFG -- the register get_cmd_stretch() already decodes. Reported raw
# here because that is what the reference tool shows; the decoded command
# rate stays on System Info.
MISC_GS_CONFIG_OFFSET = 0xE088
MISC_GS_CONFIG_FIELDS = (
    ("CMD_STRETCH", 3, 2),
    ("N_TO_1_RATIO", 5, 3),
)

# Feature switches, each in its own register, in display order. The final
# flag inverts the bit: the map calls that field "dis_pt_it", so the bit
# disables the timeout and a clear bit means it is running. Only that row
# inverts, which is why the flag is carried per row rather than assumed.
MISC_FEATURE_FIELDS = (
    # The reference tools call this one runbusy.
    ("Realtime Memory", 0x5E00, 31, 1, False),
    # Bit 40 of the 64-bit register at 0xE040, which is bit 8 of 0xE044.
    ("Power Down", 0xE044, 8, 1, False),
    ("Error Correction", 0xD804, 12, 2, False),
    ("Self Refresh", 0xD860, 16, 1, False),
    ("Memory Scrambler", 0x3E00, 0, 1, False),
    ("Row Hammer", 0xE0F8, 0, 1, False),
    ("Page Close Idle Timeout", 0xE028, 6, 1, True),
)

# 2N Mode is MR2 bit 2, reached through the pointer path rather than by
# indexing the table directly: an entry's data byte names the mode register
# and its index byte is an offset into the payload array at 0xE200. That is
# the path the DFE rows already use, and it checks out here -- MR0 resolves
# to 0x1C, which decodes to BL16 and CL 36, and 36 is exactly the tCL this
# tool reads from the timing registers.
#
# The decode tables below are the reference tools' own rather than anything
# derived from the JEDEC encoding: read_pre names two different two-clock
# patterns at codes 1 and 2, and write_pre has no entry for code 0 at all.
# Neither is something a reading of the standard would have produced, so a
# code the tables do not name is shown as a bare number instead of guessed.
MISC_READ_PREAMBLE = {
    0: "1 tCK - 10 Pattern",
    1: "2 tCK - 0010 Pattern",
    2: "2 tCK - 1110 Pattern",
    3: "3 tCK - 000010 Pattern",
    4: "4 tCK - 00001010 Pattern",
}
MISC_WRITE_PREAMBLE = {
    1: "2 tCK - 0010 Pattern",
    2: "3 tCK - 000010 Pattern",
    3: "4 tCK - 00001010 Pattern",
}
MISC_POSTAMBLE = {0: "0.5 tCK - 0 Pattern", 1: "1.5 tCK - 010 Pattern"}
# Named states rather than a switch: the cleared bit is a mode the DRAM is
# in, not a feature that is off.
MISC_READ_PREAMBLE_TRAINING = {0: "Normal Mode", 1: "Read Preamble Training"}

# (row, mode register, bit start, bit length, decode table or None)
MISC_MODE_REGISTER_FIELDS = (
    ("Read Preamble Training", 0x02, 0, 1, MISC_READ_PREAMBLE_TRAINING),
    ("Read Preamble", 0x08, 0, 3, MISC_READ_PREAMBLE),
    ("Write Preamble", 0x08, 3, 2, MISC_WRITE_PREAMBLE),
    ("Read Postamble", 0x08, 6, 1, MISC_POSTAMBLE),
    ("Write Postamble", 0x08, 7, 1, MISC_POSTAMBLE),
)

# Burst Length, MR0[1:0]. Names from the reference tools' table rather than
# from the JEDEC encoding: code 1 is the on-the-fly eight-burst and code 2 is
# the optional thirty-two, which is not the order a decode from memory would
# have produced.
#
# This read N/A for a while and was pulled. The fault was on our side: the
# lookup matched the table entry by its index and returned the data byte,
# which finds nothing for MR0. Matching on the data byte and following the
# index into the payload array is what the reference tools do, and it lands
# on 0x1C -- BL16 with CL 36, and 36 is exactly the tCL read from the timing
# registers, which is what makes the path trustworthy rather than plausible.
MISC_BURST_LENGTHS = {
    0: "BL16",
    1: "BC8 OTF",
    2: "BL32 (Optional)",
    3: "BL32 OTF (Optional)",
}
MISC_BURST_LENGTH_FIELD = (0x00, 0, 2, MISC_BURST_LENGTHS)


def _read_misc_field(offset, bit_start, bit_length, base=None):
    """Read one Misc field, or None when the register is not readable."""
    try:
        start = MCHBAR if base is None else base
        raw = read_physical_memory_int(start + offset, 4)
        if raw is None or int(raw) == 0xFFFFFFFF:
            return None
        return (int(raw) >> bit_start) & ((1 << bit_length) - 1)
    except Exception as e:
        print(f"Error reading Misc field at 0x{offset:X}: {e}")
        return None


def _misc_number(offset, bit_start, bit_length, base=None):
    value = _read_misc_field(offset, bit_start, bit_length, base)
    return "N/A" if value is None else str(value)


def _misc_switch(offset, bit_start, bit_length, inverted, base=None):
    value = _read_misc_field(offset, bit_start, bit_length, base)
    if value is None:
        return "N/A"
    return "Enabled" if bool(value) != bool(inverted) else "Disabled"


# The rest of what the mode-register table carries. 79 registers are
# reachable and these are the fields the reference map names in them that no
# other row already shows. All decode tables are the map's own.
MR_REFRESH_MODE = {0: "Normal Refresh Mode", 1: "Fine Granularity Refresh"}
MR_SUPPORTED = {0: "Not Supported", 1: "Supported"}
MR_ENABLED = {0: "Disable", 1: "Enable"}
MR_DATA_OUTPUT = {0: "Normal Operation", 1: "Disabled"}
MR_GEARDOWN = {0: "Disabled", 1: "Enabled"}
MR_CLOCK_SYNC = {0: "Not supported", 1: "Supported"}
MR_ECS_MODE = {0: "Manual ECS Mode Disabled", 1: "Manual ECS Mode Enabled"}
MR_ECS_RESET = {0: "Normal", 1: "Reset ECC Counter"}
MR_ECS_COUNTS = {
    0: "ECS counts Rows with errors",
    1: "ECS counts Code words with errors",
}
MR_READ_DQS_OFFSET = {
    0: "0 Clock", 1: "1 Clock", 2: "2 Clocks", 3: "3 Clocks",
}
# MR6 keeps the DRAM's own write recovery and read-to-precharge, which are
# not the controller's tWR and tRTP on the Timings tab -- same names, one
# programmed into the module and one applied by the controller.
MR_TWR = {
    0: "48", 1: "54", 2: "60", 3: "66", 4: "72", 5: "78", 6: "84", 7: "90",
    8: "96", 9: "Reserved", 10: "Reserved", 11: "Reserved", 12: "Reserved",
    13: "Reserved", 14: "Reserved", 15: "Reserved",
}
MR_TRTP = {
    0: "12", 1: "14", 2: "15", 3: "17", 4: "18", 5: "20", 6: "21", 7: "23",
    8: "24", 9: "Reserved", 10: "Reserved", 11: "Reserved", 12: "Reserved",
    13: "Reserved", 14: "Reserved", 15: "Reserved",
}

# MR45's table is 256 entries, but it is four runs stepping 16 clocks that
# restart at codes 64, 128 and 192 on 2048, 4096 and 8192. Written as the
# rule rather than the table: 256 transcribed lines is 256 chances to fumble
# one, and the rule reproduces every entry exactly.
MISC_DQS_TIMER_BLOCKS = ((192, 8192), (128, 4096), (64, 2048), (1, 0))
MISC_DQS_TIMER_REGISTER = 0x2D


def _ordinal(number):
    """1st, 2nd, 3rd, 4th -- and 12th rather than 12nd."""
    if 10 <= number % 100 <= 20:
        return "%dth" % number
    return "%d%s" % (number, {1: "st", 2: "nd", 3: "rd"}.get(number % 10, "th"))


def _dqs_interval_timer(code):
    """MR45 read the way the reference table reads it."""
    if code == 0:
        return "Timer Stops via MPC Command"
    for start, base in MISC_DQS_TIMER_BLOCKS:
        if code >= start:
            return "Timer Stops at %s clocks" % _ordinal(
                base + 16 * (code - start))
    return str(code)


# (row, mode register, bit start, bit length, decode table or None)
MISC_MODE_REGISTER_STATE = (
    ("Refresh tRFC Mode", 0x04, 4, 1, MR_REFRESH_MODE),
    ("Wide Range", 0x04, 5, 1, MR_SUPPORTED),
    ("Data Output Disable", 0x05, 0, 1, MR_DATA_OUTPUT),
    ("Package Output Driver Test Mode", 0x05, 3, 1, MR_SUPPORTED),
    ("TDQS Enable", 0x05, 4, 1, MR_ENABLED),
    ("DM Enable", 0x05, 5, 1, MR_ENABLED),
    ("tWR_MR", 0x06, 0, 4, MR_TWR),
    ("tRTP_MR", 0x06, 4, 4, MR_TRTP),
    ("Read DQS Offset Timing", 0x28, 0, 3, MR_READ_DQS_OFFSET),
)

# Command-bus behaviour, from the register tCCD_L comes out of.
MISC_MODE_REGISTER_COMMAND = (
    ("CS Geardown", 0x0D, 5, 1, MR_GEARDOWN),
    ("SRX/NOP Clock-Sync Support", 0x0D, 6, 1, MR_CLOCK_SYNC),
)

# Error check and scrub, all of MR14.
MISC_MODE_REGISTER_ECS = (
    ("ECS Mode", 0x0E, 7, 1, MR_ECS_MODE),
    ("ECS Reset Counter", 0x0E, 6, 1, MR_ECS_RESET),
    ("ECS Counts", 0x0E, 5, 1, MR_ECS_COUNTS),
    ("ECS Error Register Index", 0x0E, 0, 4, None),
)


def _read_misc_mode_register_field(number, bit_start, bit_length, base=None):
    """Read one mode-register field, or None when the table has no entry."""
    try:
        address = _mode_register_pointer(number, base=base)
        if address is None:
            return None
        raw = read_physical_memory_int(address, 1)
        if raw is None:
            return None
        return (int(raw) >> bit_start) & ((1 << bit_length) - 1)
    except Exception as e:
        print(f"Error reading mode register 0x{number:02X}: {e}")
        return None


def _misc_mode_register_code(number, bit_start, bit_length, decode,
                             base=None):
    """A mode-register field decoded by a function rather than a table."""
    value = _read_misc_mode_register_field(number, bit_start, bit_length, base)
    return "N/A" if value is None else decode(value)


def _misc_mode_register_value(number, bit_start, bit_length, decode,
                              base=None):
    """One mode-register field, named by its table or shown as a number.

    No table means the field is a count or an index, not a switch. It read
    as Enabled/Disabled once, which turned the ECS error register index --
    four bits selecting which record to read -- into "Disabled" at index 0.
    Every field that is a switch carries the table that says so.
    """
    value = _read_misc_mode_register_field(number, bit_start, bit_length, base)
    if value is None:
        return "N/A"
    if decode is None:
        return str(value)
    return decode.get(value, str(value))


# Misc rows naming a field DDR5 introduced, checked one at a time against
# JESD79-4 rather than dropped as a block. DDR4 does have preamble training
# and both preambles (MR4 bits 10-12), fine granularity refresh (MR3[8:6]),
# Qoff (MR1[12]), TDQS (MR1[11]), DM (MR5[10]), gear-down (MR3[3]) and the
# DRAM's own WR/RTP (MR0[13,11:9]) -- those rows name something the module
# really has, so they stay and read nothing only because this board never
# fills the controller's mode-register table.
#
# The ones below have no DDR4 counterpart at all: the postambles are fixed
# at 0.5 tCK with no register behind them, ECS is DDR5 on-die-ECC scrubbing
# on a part that has no on-die ECC, and the rest are DDR5 additions.
DDR5_ONLY_MISC_ROWS = (
    "Read Postamble",
    "Write Postamble",
    "Wide Range",
    "Package Output Driver Test Mode",
    "Read DQS Offset Timing",
    "DQS Interval Timer RT",
    "SRX/NOP Clock-Sync Support",
    "ECS Mode",
    "ECS Reset Counter",
    "ECS Counts",
    "ECS Error Register Index",
)

# Rows DDR4 does have and this cannot reach. Kept apart from the list above
# so the two never read as the same statement: those fields do not exist on
# DDR4, these do and are not readable, which is the same distinction the
# sensor tab draws for VCCIO.
#
# JESD79-4 puts all three in MR4 -- read preamble training at A10, read
# preamble at A11, write preamble at A12, each selecting one clock or two.
# They are exactly the tRPRE and tWPRE a BIOS exposes, so the fields are as
# real here as on DDR5.
#
# What is missing is the bit positions, not the register: MR4 reads 0x0008 in
# the shadow, and bit 3 is the only one set. Every other mode register decoded
# against something independent -- MR0 against tCL and tWR, MR3 against the
# refresh mode, MR6 against the BIOS tCCD_L -- and MR4 has nothing to check
# against while all three of its preamble bits read zero. A layout that fits
# three zeroes is not a layout that has been confirmed.
#
# One controlled change settles it. Set Read Preamble to 2 tCK in the BIOS and
# reboot: if MR4 becomes 0x0808, A11 is confirmed and all three rows come back
# reading. Until then they say nothing, and a row that says nothing is better
# off the tab.
DDR4_UNREACHABLE_MISC_ROWS = (
    "Read Preamble Training",
    "Read Preamble",
    "Write Preamble",
)

# The mirror of the list above: rows DDR4 has a distinct register for and
# DDR5 does not, so on DDR5 they restate a row already on the Timings tab.
#
# Refresh tRFC Mode is the only one. On DDR4 it reads MR3[8:6] -- what the
# DRAM was commanded, five states including the on-the-fly ones -- while the
# Timings row reads DDR_PTM_CTL[3:2] at MCHBAR + 0x5880, what the controller
# decided. Those two can disagree, and seeing that they do is the point.
#
# DDR5 has no separate controller policy register. Both rows resolve to MR4
# bit 4 through the same shadow window: the Timings row searches 0xE600 for
# index 0x04 and reads bit 4 of the payload at 0xE200, which is what
# read_mode_register(0x04) does. Same bit, twice, under two names -- this
# bench prints "FGR Mode (tRFC2)" on Timings and "Fine Granularity Refresh"
# on Misc. The Timings row is the one that stays: it heads the Refresh
# timings section and names the interval that applies, directly above the
# tRFC/tRFC2/tRFCpb rows it applies to.
DDR4_ONLY_MISC_ROWS = (
    "Refresh tRFC Mode",
)


def _misc_rows_for_generation(rows, generation):
    """The Misc rows one memory generation shows, from the full set.

    Pure, and the only place the three gating lists are applied, so what a
    generation drops can be checked without building the tab.
    """
    if generation == "DDR4":
        dropped = set(DDR5_ONLY_MISC_ROWS) | set(DDR4_UNREACHABLE_MISC_ROWS)
    else:
        dropped = set(DDR4_ONLY_MISC_ROWS)
    return [row for row in rows if row["name"] not in dropped]

# The rows DDR4 does have, read out of the mode-register shadow instead of
# the 0xE600 table DDR4 never fills. Same fields, different registers and
# different encodings -- DDR4 tops out at BL8 and its write recovery counts
# from 10, not 48, so reusing the DDR5 tables here would print DDR5 numbers.
DDR4_MR_BURST_LENGTHS = {
    0: "BL8 (Fixed)",
    1: "BC4 or BL8 (On The Fly)",
    2: "BC4 (Fixed)",
}
DDR4_MR_REFRESH_MODE = {
    0: "Normal Refresh Mode",
    1: "Fixed 2x",
    2: "Fixed 4x",
    5: "On The Fly 2x",
    6: "On The Fly 4x",
}
# MR0 pairs write recovery with read-to-precharge in one code, split across
# A13 and A[11:9]. Code 6 reads WR 24 here, which is what the tWR row shows.
DDR4_MR_WR_RTP = {
    0: (10, 5), 1: (12, 6), 2: (14, 7), 3: (16, 8), 4: (18, 9),
    5: (20, 10), 6: (24, 12), 7: (22, 11), 8: (26, 13), 9: (28, 14),
}


def _ddr4_wr_rtp_code(base=None):
    """MR0's write-recovery code, whose two halves are not adjacent."""
    value = _ddr4_mode_register(0x00, base)
    if value is None:
        return None
    return ((value >> 13) & 1) << 3 | ((value >> 9) & 0x7)


def _ddr4_wr_rtp(half, base=None):
    code = _ddr4_wr_rtp_code(base)
    if code is None:
        return "N/A"
    pair = DDR4_MR_WR_RTP.get(code)
    return "N/A" if pair is None else str(pair[half])


# (row, mode register, bit start, bit length, decode table)
DDR4_MISC_MODE_REGISTER_FIELDS = (
    ("Burst Length", 0x00, 0, 2, DDR4_MR_BURST_LENGTHS),
    ("TDQS Enable", 0x01, 11, 1, MR_ENABLED),
    ("Data Output Disable", 0x01, 12, 1, MR_DATA_OUTPUT),
    ("CS Geardown", 0x03, 3, 1, MR_GEARDOWN),
    ("Refresh tRFC Mode", 0x03, 6, 3, DDR4_MR_REFRESH_MODE),
    ("DM Enable", 0x05, 10, 1, MR_ENABLED),
)


def _ddr4_misc_value(number, bit_start, bit_length, decode, base=None):
    """One Misc row read out of the DDR4 shadow."""
    value = _ddr4_mode_register_field(number, bit_start, bit_length, base)
    if value is None:
        return "N/A"
    return decode.get(value, str(value))


def _misc_row(name, category, column, value):
    return {
        "name": name,
        "Category": category,
        "Tab": MISC_TAB,
        "Column": column,
        "value": value,
        "read_type": "standard",
    }


def _install_misc_tab():
    """Add the Misc tab: CKE/power-down control, command config, features.

    Held to the platforms the map was built for. Arrow Lake moved several of
    these registers -- CMD_STRETCH alone went from two bits to one -- so the
    same offsets there would read plausible numbers that mean something else.
    """
    if is_arrow_lake_platform():
        return

    # Every row carries a getter, not a reading. These registers change while
    # the tool is open -- the CKE block and the feature switches follow what
    # the controller is doing -- so a value resolved here would be a snapshot
    # of startup that never moved again.
    #
    # One reading per row, from channel A. The 0xE000 block and the
    # mode-register table do have channel-B twins, and these rows carried
    # both for a while -- but two columns leave the value column too narrow
    # for what this tab holds, and "Manual ECS Mode Disabled", "Fine
    # Granularity Refresh" and the preamble patterns were all cut off. These
    # are controller settings that read the same on both channels anyway.
    def cke(bit_start, bit_length):
        return lambda base: _misc_number(
            MISC_CKE_CONFIG_OFFSET, bit_start, bit_length, base)

    def gs(bit_start, bit_length):
        return lambda base: _misc_number(
            MISC_GS_CONFIG_OFFSET, bit_start, bit_length, base)

    def switch(offset, bit_start, bit_length, inverted):
        return lambda base: _misc_switch(
            offset, bit_start, bit_length, inverted, base)

    def _channel_a(read):
        """Bind a base-taking reader to channel A."""
        return lambda: read(MCHBAR)

    def mode_register(number, bit_start, bit_length, decode):
        return lambda base: _misc_mode_register_value(
            number, bit_start, bit_length, decode, base)

    rows = [
        _misc_row(name, "Power Down", "Right",
                  _channel_a(cke(bit_start, bit_length)))
        for name, bit_start, bit_length in MISC_CKE_CONFIG_FIELDS
    ]

    rows.extend(
        _misc_row(name, "Command", "Right",
                  _channel_a(gs(bit_start, bit_length)))
        for name, bit_start, bit_length in MISC_GS_CONFIG_FIELDS
    )
    # Burst Length sits with the command configuration, where the reference
    # tools list it, even though it comes from a mode register.
    rows.append(_misc_row(
        "Burst Length", "Command", "Right",
        _channel_a(mode_register(*MISC_BURST_LENGTH_FIELD))))
    rows.extend(
        _misc_row(name, "Command", "Right",
                  _channel_a(mode_register(number, bit_start, bit_length,
                                           decode)))
        for name, number, bit_start, bit_length, decode
        in MISC_MODE_REGISTER_COMMAND
    )

    rows.extend(
        _misc_row(name, "ECS", "Left",
                  _channel_a(mode_register(number, bit_start, bit_length,
                                           decode)))
        for name, number, bit_start, bit_length, decode
        in MISC_MODE_REGISTER_ECS
    )

    rows.extend(
        _misc_row(name, "Preamble", "Left",
                  _channel_a(mode_register(number, bit_start, bit_length,
                                           decode)))
        for name, number, bit_start, bit_length, decode
        in MISC_MODE_REGISTER_FIELDS
    )

    for name, number, bit_start, bit_length, decode in MISC_MODE_REGISTER_STATE:
        read = mode_register(number, bit_start, bit_length, decode)
        row = _misc_row(name, "Mode Registers", "Left", _channel_a(read))
        # Kept unbound as well: tWR_MR and tRTP_MR move to the Timings
        # tab, where every row reads both controllers, and a getter
        # already bound to channel A cannot be asked for channel B.
        row["base_reader"] = read
        rows.append(row)
    rows.append(_misc_row(
        "DQS Interval Timer RT", "Mode Registers", "Left",
        lambda: _misc_mode_register_code(
            MISC_DQS_TIMER_REGISTER, 0, 8, _dqs_interval_timer)))


    rows.extend(
        _misc_row(name, "Features", "Right",
                  _channel_a(switch(offset, bit_start, bit_length, inverted)))
        for name, offset, bit_start, bit_length, inverted in MISC_FEATURE_FIELDS
    )


    generation = detect_ddr_generation()
    rows = _misc_rows_for_generation(rows, generation)

    if generation == "DDR4":
        # Re-point what DDR4 does carry at the shadow. The three preamble
        # rows are not in here: they live in MR4, where only one bit is set
        # on this bench and one bit is not enough to say which field it is.
        # They keep reporting nothing until a BIOS change says which moves.
        ddr4 = {
            name: (lambda number, start, length, table:
                   lambda base: _ddr4_misc_value(
                       number, start, length, table, base))(*field)
            for name, *field in DDR4_MISC_MODE_REGISTER_FIELDS
        }
        ddr4["tWR_MR"] = lambda base: _ddr4_wr_rtp(0, base)
        ddr4["tRTP_MR"] = lambda base: _ddr4_wr_rtp(1, base)
        for row in rows:
            read = ddr4.get(row["name"])
            if read is not None:
                row["value"] = _channel_a(read)
                row["base_reader"] = read

    # The round-trip latencies move in rather than holding a tab of their own.
    # They keep their Latency CHA/CHB headings, so the two blocks stay separate
    # on the page; only the tab strip loses an entry. Done here, inside the
    # guard, so a platform that gets no Misc rows keeps its RTL tab under its
    # own name instead of one called Misc that holds nothing but latencies.
    for timing in TIMINGS:
        if timing.get("Tab") == RTL_TAB:
            timing["Tab"] = MISC_TAB

    TIMINGS.extend(rows)


_install_misc_tab()


# --- tWR_MR and tRTP_MR belong with the timings they restate.
#
# Both are the mode register's own copy of a timing the controller also holds:
# tWR beside tWR, tRTP beside tRTP. They were built with the rest of the mode
# registers on Misc, which is where they are read from, not where they are
# read for.
#
# Moved after the fact rather than built elsewhere, so they keep the getters
# the Misc builder gave them -- the same decode, and the DDR4 re-point that
# follows it.
def _move_mode_register_timings():
    """Put the mode-register copies of tWR and tRTP on the Timings tab."""
    moved = []
    for name in ("tWR_MR", "tRTP_MR"):
        row = next(
            (t for t in TIMINGS
             if t.get("name") == name and t.get("Tab") == MISC_TAB),
            None,
        )
        if row is None:
            continue
        TIMINGS.remove(row)
        row.update({"Tab": "Timings", "Category": "Secondary",
                    "Column": "Left"})
        reader = row.pop("base_reader", None)
        if reader is not None:
            # Every row on this tab reads both controllers.
            _promote_computed_row(row, reader)
        moved.append(row)
    if not moved:
        return
    # Each one directly under the row it restates: tWR_MR below tWR,
    # tRTP_MR below tRTP.
    for row in moved:
        under = row["name"].replace("_MR", "")
        anchor = next(
            (i for i, t in enumerate(TIMINGS)
             if t.get("name") == under and t.get("Tab") == "Timings"),
            None,
        )
        if anchor is None:
            TIMINGS.append(row)
        else:
            TIMINGS.insert(anchor + 1, row)


_move_mode_register_timings()


# --- The refresh arbitration rows read as controller policy, not as timings.
#
# What the controller does about refresh -- when it may skip a per-bank one,
# how many it will bank up before it insists -- rather than how long any of
# them takes. They are read from the same two registers as OREF_RI and
# tRFCpb, which is why they were built beside them.
#
# PBR Exit on idle stays on Timings: it was not among those asked for, and
# moving it because its neighbours moved would be inventing the request.
REFRESH_POLICY_ROWS = (
    "PBR Disable", "PBR OOO Disable", "PBR Disable on hot",
    "PBR Exit on idle", "Refresh ABR release", "Refresh HP WM",
    "Refresh panic WM", "CounttREFIWhileRefEnOff", "HPRefOnMRS",
    "SRX_Ref_Debits", "RAISE_BLK_WAIT",
)


def _move_refresh_policy_rows():
    """Put the refresh arbitration controls on Misc, in their own section."""
    moved = []
    for name in REFRESH_POLICY_ROWS:
        row = next(
            (t for t in TIMINGS
             if t.get("name") == name and t.get("Tab") == "Timings"),
            None,
        )
        if row is None:
            continue
        TIMINGS.remove(row)
        # Left: the right column already carries Power Down, Command and
        # Features, and eleven more there would leave the columns 33 and 50.
        row.update({"Tab": MISC_TAB, "Category": "Refresh", "Column": "Left"})
        # Misc shows one value per row by design: these registers do
        # have a channel-B twin, but it holds the same value and a
        # second column leaves the text this tab carries too narrow.
        # The channel-A address the row already has is the reading.
        for key in ("address_a", "address_b", "parameters_a",
                    "parameters_b", "read_type_a", "read_type_b",
                    "value_a", "value_b", "name_a", "name_b",
                    "parameter_name", "dynamic_params_a",
                    "dynamic_params_b"):
            row.pop(key, None)
        moved.append(row)
    TIMINGS.extend(moved)


_move_refresh_policy_rows()


# --- Misc draws as a single column.
#
# The tab held two, which suited it while it was short. It is 83 rows now and
# the split was doing less than it looks: the two halves have to be levelled
# to the same height for the row shading to cross the tab whole, so the
# shorter one was padded with blank rows, and reading down one column then
# back up to the top of the other is a worse way through a list of settings
# than reading straight down.
#
# Order follows the table, which puts the two latency blocks together at the
# head and the refresh policy at the foot.
#
# The column stays named rather than removed: the renderer builds both halves
# for every tab, and a tab with nothing in its right half draws it empty,
# which is what one column means here.
def _collapse_misc_to_one_column():
    for timing in TIMINGS:
        if timing.get("Tab") == MISC_TAB:
            timing["Column"] = "Left"


_collapse_misc_to_one_column()


# --- DDR5 row names.
#
# Deliberately the last installer in this module. See DDR5_TIMING_LABELS:
# every pass above matches rows by their declared name, so renaming any
# earlier would make those passes miss the rows they are looking for.
def _install_ddr5_timing_labels():
    platform = active_platform()
    for timing in TIMINGS:
        name = timing.get("name")
        label = ddr5_timing_label(platform, name)
        if label != name:
            timing["name"] = label


_install_ddr5_timing_labels()
