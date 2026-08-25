# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio
#
# This file follows ZenStates-Core and ZenTimings by irusanov
# (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
# and the bounds applied to decoded values were taken from or checked against
# that work, and the comments below say where. Copyright in those parts
# remains with their authors; this file is distributed under the same licence
# they are, which is what makes that use permitted.
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

"""DDR5 PMIC rail decoding (JESD301 layout).

The DIMM's PMIC owns VDD, VDDQ and VPP.  The CPU SMU does not report them —
not reachable any other way on this platform -- so these
registers are the only source.

Register and encoding constants below are the JEDEC-documented layout, but
whether this board's PMIC answers with them is a hardware question:
``CONFIRMED_PMIC_RAILS`` stays empty until ``ddr5_pmic_probe`` has matched the
decoded values against a known-good monitor on real hardware.  Same evidence
rule as the PM-table offsets, for the same reason — a wrong decode prints a
believable but false voltage.
"""

from __future__ import annotations

# Believable ranges live with the rail definitions, so the transport and the
# view can never disagree about what counts as a plausible reading.
from rochviewer.sensors.voltage_rails import RAILS_BY_KEY, validate_voltage

# VID (voltage-ID) registers — the configured rail setting.
# Register numbers confirmed on MSI B850MPOWER + G.Skill F5-6000J2636G16G by
# matching HWiNFO at two profiles (see CONFIRMED_PMIC_RAILS); formulas from
# the ZenStates-Core Ddr5PmicDecoder reference implementation.
SWA_VDD_REGISTER = 0x21       # SWA -> VDD
SWB_VDDQ_REGISTER = 0x25      # SWB -> VDDQ
SWC_VPP_REGISTER = 0x27       # SWC -> VPP

# JESD301 VID encoding.  SWA/SWB sit above a 800 mV floor, SWC above 1500 mV,
# both in 5 mV steps.  Two modes exist and the register itself does not say
# which is active:
#   7-bit JEDEC : mV = base + ((reg >> 1) & 0x7F) * 5
#   8-bit OC    : mV = base + reg * 5          (vendor overclocking extension)
SWA_SWB_BASE_MV = 800
SWC_BASE_MV = 1500
VID_STEP_MV = 5

# Registers dumped by the research probe. Reading is harmless, so the window
# is wide enough to catch rail registers outside the documented block.
PMIC_REGISTER_SCAN = tuple(range(0x00, 0x80))

# Mode register.  The VID registers do not record whether the vendor 8-bit
# overclocking extension is active, so it is read from here.
#
# Confirmed by capturing at JEDEC 4800 and again at 8200 MT/s: R2Bh moved
# 0x42 -> 0x72, i.e. bits 4 and 5 set, exactly when SWA and SWB switched from
# the 7-bit to the 8-bit decode.  SWC stayed 7-bit at both profiles.
VID_MODE_REGISTER = 0x2B
VID_MODE_SWA_8BIT = 1 << 4
VID_MODE_SWB_8BIT = 1 << 5

# rail key -> (VID register, base mV, 8-bit mode mask or None for always 7-bit)
#
# Confirmed on MSI B850MPOWER + Ryzen 7 9850X3D + G.Skill F5-6000J2636G16G,
# PMIC 0x49/0x4B, against HWiNFO 8.44 and ZenTimings 1.43.0 at TWO profiles:
#
#                     JEDEC 4800            8200 MT/s
#   R21h SWA   0x78 -> 1.100 V (7-bit)  0x8C -> 1.500 V (8-bit)   HWiNFO 1.500
#   R25h SWB   0x78 -> 1.100 V (7-bit)  0x80 -> 1.440 V (8-bit)   ZenT  1.4400
#   R27h SWC   0x78 -> 1.800 V (7-bit)  0x78 -> 1.800 V (7-bit)   both  1.800
#
# Formulas from the ZenStates-Core Ddr5PmicDecoder reference implementation:
#   7-bit JEDEC : mV = base + ((reg >> 1) & 0x7F) * 5
#   8-bit OC    : mV = base + reg * 5
CONFIRMED_PMIC_RAILS = {
    "dram_vdd": (SWA_VDD_REGISTER, SWA_SWB_BASE_MV, VID_MODE_SWA_8BIT),
    "dram_vddq": (SWB_VDDQ_REGISTER, SWA_SWB_BASE_MV, VID_MODE_SWB_8BIT),
    "dram_vpp": (SWC_VPP_REGISTER, SWC_BASE_MV, None),
}


def decode_rails(read_register, rails=None):
    """Decode the confirmed DRAM rails from one PMIC.

    ``read_register(register)`` returns a byte.  Rails that decode outside
    their believable range are dropped rather than displayed.
    """
    rails = CONFIRMED_PMIC_RAILS if rails is None else rails
    try:
        mode = int(read_register(VID_MODE_REGISTER)) & 0xFF
    except (OSError, TimeoutError, ValueError):
        return {}
    values = {}
    for key, (register, base_mv, mask) in rails.items():
        try:
            raw = int(read_register(register)) & 0xFF
        except (OSError, TimeoutError, ValueError):
            continue
        eight_bit = bool(mask is not None and mode & mask)
        volts = (
            decode_vid_8bit(raw, base_mv) if eight_bit
            else decode_vid_7bit(raw, base_mv)
        )
        rail = RAILS_BY_KEY.get(key)
        if rail is None:
            continue
        try:
            values[key] = validate_voltage(rail, volts)
        except ValueError:
            continue
    return values

# --- SPD identification (JESD400-5) -----------------------------------------
#
# The SPD carries the PMIC's manufacturer and device type.  Knowing the exact
# part is what turns register archaeology into a documented lookup, so the
# probe reports these rather than pattern-matching register values.
SPD_PMIC0_MANUFACTURER_LSB = 552
SPD_PMIC0_MANUFACTURER_MSB = 553
SPD_PMIC0_DEVICE_TYPE = 554
SPD_PMIC0_REVISION = 555
SPD_MODULE_MANUFACTURER_LSB = 512
SPD_MODULE_MANUFACTURER_MSB = 513
SPD_HUB_MANUFACTURER_LSB = 320
SPD_HUB_MANUFACTURER_MSB = 321

# Bytes the probe pulls to identify the parts on the module.
SPD_IDENTITY_RANGE = (320, 8), (512, 48)

# JEP106 continuation-coded IDs seen on DDR5 PMICs.
PMIC_VENDORS = {
    0x00B3: "IDT / Renesas",
    0x0086: "Intel",
    0x001B: "Montage",
    0x00C1: "Infineon",
    0x004E: "Richtek",
    0x0098: "Analog Devices",
    0x009D: "Monolithic Power Systems",
    0x00E9: "Rohm",
    0x0031: "Texas Instruments",
}


# Which PMIC answered last time, so repeat reads skip the bus scan.
#
# This matters for responsiveness: a probe of an empty address blocks for the
# full SMBus timeout, and scanning both controllers across all eight PMIC
# addresses is mostly misses. Rediscovering that on every refresh made the UI
# stutter. The location cannot change while the machine is running.
_PMIC_LOCATION = {"reader": None, "controller": None, "address": None}


def _rails_from(reader, address, controller):
    return decode_rails(
        lambda register: reader.read_byte(address, register, controller)
    )


def read_dram_rails(reader_factory=None, controllers=None, addresses=None):
    """Read DRAM VDD/VDDQ/VPP from the first responding DIMM PMIC.

    Returns ``{rail key: volts}``; empty when no PMIC answers.  Both DIMMs are
    driven from the same board setting, so the first one that responds is
    representative.

    The decode is JEDEC and the same on any platform; only the bus underneath
    differs.  An Intel caller passes ``intel_pch_smbus``'s reader along with
    its own controller and address lists, and nothing here changes.  Defaults
    stay the AM5 ones so existing callers are untouched.
    """
    if not CONFIRMED_PMIC_RAILS:
        return {}
    try:
        # The host controller differs per platform and the PMIC does not, so
        # the transport comes from the dispatcher rather than being named
        # here. It used to be hardcoded -- to the AMD one while this was an
        # AM5 tool, to the Intel one while the tree was Intel-only -- and
        # either way it was right by accident, because every caller in the
        # app passes a factory in.
        if reader_factory is None or controllers is None or addresses is None:
            from rochviewer.memory.ddr5_telemetry import default_smbus_backend

            backend = default_smbus_backend()
            if backend is None and reader_factory is None:
                return {}
            if backend is None:
                # A caller that supplied a reader supplied the transport.
                # Only the addresses are missing, and those are JEDEC's --
                # PMICs answer at 0x48-0x4F behind any host controller.
                # Bailing here regardless discarded the caller's reader,
                # which is the same mistake read_identity used to make.
                from rochviewer.intel.intel_pch_smbus import (
                    CONTROLLER_OFFSETS as offsets,
                    PMIC_ADDRESSES as pmics,
                )

                default_factory = None
            else:
                default_factory, offsets, _hubs, pmics = backend
            reader_factory = reader_factory or default_factory
            controllers = offsets if controllers is None else controllers
            addresses = pmics if addresses is None else addresses

        cached = _PMIC_LOCATION
        if cached["reader"] is not None:
            values = _rails_from(
                cached["reader"], cached["address"], cached["controller"]
            )
            if values:
                return values
            # The DIMM stopped answering; fall through and rescan once.
            cached["reader"] = None

        reader = reader_factory()
        if not reader.is_driver_open():
            return {}
        for controller in controllers:
            for address in addresses:
                values = _rails_from(reader, address, controller)
                if values:
                    _PMIC_LOCATION.update(
                        reader=reader, controller=controller, address=address
                    )
                    return values
    except Exception:
        _PMIC_LOCATION["reader"] = None
        return {}
    return {}


# SPD5 hub temperature sensor. MR49/MR50 hold a 16-bit reading whose bits
# [12:2] are an 11-bit signed value at 0.25 C per LSB. Register numbers and the
# conversion follow the ZenStates-Core Ddr5ThermalSensor reference.
SPD_TEMPERATURE_REGISTER = 0x31        # MR49, low byte; MR50 follows at 0x32
SPD_TEMPERATURE_STEP_C = 0.25
SPD_HUB_BASE_ADDRESS = 0x50            # first slot of channel A
DIMM_TEMPERATURE_MIN_C = 0.0
DIMM_TEMPERATURE_MAX_C = 125.0

# SPD5 hub identity, MR0/MR1. An SPD5118-class hub answers 0x51/0x18 there,
# confirmed on the Z790 bench where both populated slots reported exactly that.
#
# Asked before the temperature is believed, and the reason is specific: a DDR4
# module's SPD EEPROM lives at these same 0x50-0x57 addresses. It answers a
# byte read perfectly happily, and MR49/MR50 would then land on two ordinary
# SPD bytes whose value decodes to a number the band below cannot reject --
# a DDR4 board reporting a confident, invented DIMM temperature. Two reads
# settle what the device actually is, which is the difference between a
# reading and a coincidence.
SPD_HUB_DEVICE_TYPE_REGISTER = 0x00    # MR0; MR1 follows at 0x01
SPD_HUB_DEVICE_TYPE = (0x51, 0x18)


def is_spd5_hub(read_register):
    """True when the device at this address identifies as an SPD5 hub."""
    try:
        low = int(read_register(SPD_HUB_DEVICE_TYPE_REGISTER)) & 0xFF
        high = int(read_register(SPD_HUB_DEVICE_TYPE_REGISTER + 1)) & 0xFF
    except (OSError, TimeoutError, ValueError):
        return False
    return (low, high) == SPD_HUB_DEVICE_TYPE


def decode_spd_temperature(low, high):
    """Decode the MR49/MR50 pair into degrees Celsius."""
    raw = ((int(high) & 0xFF) << 8) | (int(low) & 0xFF)
    value = (raw >> 2) & 0x7FF
    if value & 0x400:                  # 11-bit sign bit
        value -= 0x800
    return value * SPD_TEMPERATURE_STEP_C


def spd_hub_channel(address):
    """Return the channel an SPD hub address belongs to.

    DDR5 slots take one SMBus address each, in slot order: 0x50 and 0x51 are
    the two channel-A slots, 0x52 and 0x53 the two channel-B slots. The bench
    board answers on 0x51 and 0x53, one per channel, which is what a
    two-DIMM AM5 build looks like.
    """
    index = (int(address) - SPD_HUB_BASE_ADDRESS) // 2
    return "ab"[index] if 0 <= index < 2 else None


def read_dimm_temperatures(reader_factory=None, controllers=None,
                           addresses=None):
    """Read every DIMM's own SPD hub temperature sensor.

    The only readings in this project that come from the modules rather than
    the CPU or the board. Returns ``{channel: celsius}`` keyed "a"/"b", and an
    empty dict when no hub answers.

    Every populated hub is read, not just the first one to reply: two DIMMs
    sit in different airflow and routinely differ by a couple of degrees, so
    one number cannot stand for both.

    An address is only believed once the device there identifies as an SPD5
    hub; see SPD_HUB_DEVICE_TYPE. The check runs during discovery only, since
    a module cannot be swapped while the machine is running and repeating it
    on every refresh would double the bus traffic behind these two rows.

    As with :func:`read_dram_rails`, the bus is the caller's choice and the
    dispatcher's default: the host controller differs per platform, and the
    JESD300-5 decode below does not.
    """
    temperatures = {}
    try:
        # Same as read_dram_rails: the dispatcher names the transport, since
        # the host controller is the platform's and the hub is not.
        if reader_factory is None or controllers is None or addresses is None:
            from rochviewer.memory.ddr5_telemetry import default_smbus_backend

            backend = default_smbus_backend()
            if backend is None and reader_factory is None:
                return {}
            if backend is None:
                # See read_dram_rails: hubs answer at 0x50-0x57 on any host
                # controller, so a supplied reader is enough.
                from rochviewer.intel.intel_pch_smbus import (
                    CONTROLLER_OFFSETS as offsets,
                    SPD_HUB_ADDRESSES as hubs,
                )

                default_factory = None
            else:
                default_factory, offsets, hubs, _pmics = backend
            reader_factory = reader_factory or default_factory
            controllers = offsets if controllers is None else controllers
            addresses = hubs if addresses is None else addresses

        cached = _SPD_LOCATIONS
        known = bool(cached["reader"] is not None and cached["locations"])
        candidates = (
            list(cached["locations"]) if known
            else [(c, a) for c in controllers for a in addresses]
        )
        reader = cached["reader"] or reader_factory()
        if not reader.is_driver_open():
            return {}
        found = []
        for controller, address in candidates:
            channel = spd_hub_channel(address)
            if channel is None or channel in temperatures:
                continue
            if not known and not is_spd5_hub(
                lambda register: reader.read_byte(address, register, controller)
            ):
                # Something answered, but it is not an SPD5 hub. On a DDR4
                # board that is the module's SPD EEPROM.
                continue
            try:
                low = reader.read_byte(
                    address, SPD_TEMPERATURE_REGISTER, controller
                )
                high = reader.read_byte(
                    address, SPD_TEMPERATURE_REGISTER + 1, controller
                )
            except (OSError, TimeoutError, ValueError):
                continue
            celsius = decode_spd_temperature(low, high)
            if DIMM_TEMPERATURE_MIN_C <= celsius <= DIMM_TEMPERATURE_MAX_C:
                temperatures[channel] = celsius
                found.append((controller, address))
        if found:
            _SPD_LOCATIONS.update(reader=reader, locations=tuple(found))
        else:
            _SPD_LOCATIONS.update(reader=None, locations=())
    except Exception:
        _SPD_LOCATIONS.update(reader=None, locations=())
        return {}
    return temperatures


def read_dimm_temperature(reader_factory=None):
    """Warmest populated DIMM, or None when no hub answers.

    Kept for callers that want one number for the set; the per-DIMM rows use
    :func:`read_dimm_temperatures`.
    """
    temperatures = read_dimm_temperatures(reader_factory)
    return max(temperatures.values()) if temperatures else None


# Which SPD hubs answered last, so repeat reads skip the scan.
_SPD_LOCATIONS = {"reader": None, "locations": ()}


def decode_jep106(lsb, msb):
    """Return ``(code, vendor name)`` for a JEP106 manufacturer pair."""
    code = ((int(lsb) & 0x7F) << 8) | (int(msb) & 0xFF)
    # Some parts store the pair the other way round; try both before giving up.
    swapped = ((int(msb) & 0x7F) << 8) | (int(lsb) & 0xFF)
    for candidate in (code, swapped):
        if candidate in PMIC_VENDORS:
            return candidate, PMIC_VENDORS[candidate]
    return code, "unknown"


def decode_vid_7bit(raw, base_mv):
    """JEDEC 7-bit VID decode, in volts."""
    return (base_mv + (((int(raw) & 0xFF) >> 1) & 0x7F) * VID_STEP_MV) / 1000.0


def decode_vid_8bit(raw, base_mv):
    """Vendor 8-bit overclocking VID decode, in volts."""
    return (base_mv + (int(raw) & 0xFF) * VID_STEP_MV) / 1000.0


def decode_swa_swb_volts(raw, eight_bit=False):
    """Decode a SWA/SWB (VDD/VDDQ) VID byte into volts."""
    decode = decode_vid_8bit if eight_bit else decode_vid_7bit
    return decode(raw, SWA_SWB_BASE_MV)


def decode_swc_volts(raw, eight_bit=False):
    """Decode a SWC (VPP) VID byte into volts."""
    decode = decode_vid_8bit if eight_bit else decode_vid_7bit
    return decode(raw, SWC_BASE_MV)



# The PMIC's ADC telemetry window is deliberately not used. It was explored as
# a way to break the 7-bit/8-bit VID tie, but VID_MODE_REGISTER answers that
# directly and carries two-profile evidence. Dropping it also leaves the SPD
# page selector as the only writable SMBus target, so no PMIC register is
# writable at all.
