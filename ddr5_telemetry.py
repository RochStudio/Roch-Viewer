"""Per-DIMM PMIC telemetry: measured rails, input voltage and power.

What :mod:`ddr5_pmic` already reads are the VID registers -- what the rails are
*set* to. This module reads what the PMIC's ADC *measures*, which is a
different thing and differs between two modules on the same board setting.

Getting a measurement costs one write. The ADC is a multiplexer: R30h selects
a channel, JESD301-2 requires ~9 ms to settle, and R31h then holds the sample.
R30h is a selector and carries no rail setting -- see the SAFETY note in
:mod:`intel_pch_smbus`, where it is one of exactly two writable targets and the
rail registers are none of them.

Decode follows ZenStates-Core's Ddr5PmicReader/Ddr5PmicDecoder, and was
checked against ZenTimings 1.43 on the bench: current mode (R1Bh bit 6 clear),
limits 6000/6000/1250 mA from R20h, raws 3/1/0 -> 0.2405 W, inside the
0.242-0.276 W the same window was reporting.
"""

from __future__ import annotations

# The ADC multiplexer: write the channel to R30h, wait, read the sample.
TELEMETRY_SELECT_REGISTER = 0x30
TELEMETRY_VALUE_REGISTER = 0x31

# JESD301-2 asks for 9 ms after selecting a channel. That is the floor, not
# enough here: at 9 ms with two DIMMs polled in turn, R31h still held the
# previous channel's conversion often enough to rotate the readings -- VDDQ
# showing VDD's value, VPP showing VDDQ's. A stale sample is indistinguishable
# from a real one, since it is a real voltage, just the wrong rail's.
#
# So: longer settle, and the value register is read twice with the first read
# discarded. The discard costs one SMBus transaction and closes the window
# where a conversion that finished during the first read is missed by it.
TELEMETRY_SETTLE_SECONDS = 0.012

# Channel codes, and the mV each LSB is worth (JESD301-2 Table 137). The rail
# channels and the two internal supplies step 15 mV; the bulk input, being a
# far wider range, steps 70 mV.
ADC_SWA_VDD = 0x0
ADC_SWB_VDDQ = 0x2
ADC_SWC_VPP = 0x3
ADC_VIN_BULK = 0x5
ADC_VOUT_1V8 = 0x8
ADC_VOUT_1V0 = 0x9

ADC_STEP_MV = {
    ADC_SWA_VDD: 15, ADC_SWB_VDDQ: 15, ADC_SWC_VPP: 15,
    ADC_VOUT_1V8: 15, ADC_VOUT_1V0: 15,
    ADC_VIN_BULK: 70,
}

# Telemetry registers. SWB/SWC report in the low six bits.
SWA_TELEMETRY_REGISTER = 0x0C
SWB_TELEMETRY_REGISTER = 0x0E
SWC_TELEMETRY_REGISTER = 0x0F
TELEMETRY_MODE_REGISTER = 0x1B          # bit 6: reports power rather than current
TELEMETRY_TOTAL_REGISTER = 0x1A         # bit 1: SWA carries the combined total
CURRENT_LIMIT_REGISTER = 0x20           # SWA [7:6], SWB [3:2], SWC [1:0]
RAIL_CONFIG_REGISTER = 0x29             # bit 3: SWA runs two phases

POWER_STEP_W = 0.125                    # JESD301-2, power mode
SWAB_CURRENT_LIMIT_MA = (3000, 4000, 5000, 6000)
SWC_CURRENT_LIMIT_MA = (500, 750, 1000, 1250)

# A sample past what the channel can physically carry is a failed conversion,
# not a reading. The DIMM rails and the two internal supplies sit under 3 V;
# the bulk input is 5 V on a UDIMM and 12 V on other module types.
ADC_MAX_MV = {ADC_VIN_BULK: 15000}
ADC_DEFAULT_MAX_MV = 3000


def decode_adc_mv(channel, raw):
    """Return millivolts for an ADC sample, or None when the channel is idle."""
    raw = int(raw) & 0xFF
    step = ADC_STEP_MV.get(channel)
    if step is None or raw == 0:
        return None
    millivolts = raw * step
    if millivolts > ADC_MAX_MV.get(channel, ADC_DEFAULT_MAX_MV):
        return None
    return millivolts


def select_byte(channel):
    """The R30h value that selects an ADC channel: enable bit plus the code."""
    return 0x80 | ((int(channel) & 0x0F) << 3)


def decode_current_limits(raw):
    """Return the three rails' configured current limits, in mA."""
    raw = int(raw) & 0xFF
    return (
        SWAB_CURRENT_LIMIT_MA[(raw >> 6) & 0x03],
        SWAB_CURRENT_LIMIT_MA[(raw >> 2) & 0x03],
        SWC_CURRENT_LIMIT_MA[raw & 0x03],
    )


def decode_power_watts(registers, volts):
    """Return ``(swa, swb, swc, total)`` watts, or None when undecodable.

    Two modes exist and the telemetry registers do not say which they are in,
    so R1Bh bit 6 does: set means they report power directly, clear means they
    report current as a fraction of the rail's configured limit and the rail
    voltage turns it into watts.
    """
    try:
        swa_raw = int(registers[SWA_TELEMETRY_REGISTER]) & 0xFF
        swb_raw = int(registers[SWB_TELEMETRY_REGISTER]) & 0x3F
        swc_raw = int(registers[SWC_TELEMETRY_REGISTER]) & 0x3F
        mode = int(registers[TELEMETRY_MODE_REGISTER])
    except (KeyError, TypeError, ValueError):
        return None

    if mode & (1 << 6):
        swa = swa_raw * POWER_STEP_W
        swb = swb_raw * POWER_STEP_W
        swc = swc_raw * POWER_STEP_W
        total_flag = int(registers.get(TELEMETRY_TOTAL_REGISTER, 0)) & (1 << 1)
        if total_flag and swa >= swb + swc:
            # SWA carries the combined figure; the rail's own share is the
            # remainder rather than the whole.
            return swa - swb - swc, swb, swc, swa
        return swa, swb, swc, swa + swb + swc

    try:
        limits = decode_current_limits(registers[CURRENT_LIMIT_REGISTER])
        phases = 2 if int(registers[RAIL_CONFIG_REGISTER]) & (1 << 3) else 1
    except (KeyError, TypeError, ValueError):
        return None

    swa_limit, swb_limit, swc_limit = limits
    # SWA's code is eight bits of its limit; SWB and SWC are six.
    swa_amps = swa_raw * (swa_limit / 1000.0) / 256.0 * phases
    swb_amps = swb_raw * (swb_limit / 1000.0) / 64.0
    swc_amps = swc_raw * (swc_limit / 1000.0) / 64.0

    swa = swa_amps * volts.get("vdd", 0.0)
    swb = swb_amps * volts.get("vddq", 0.0)
    swc = swc_amps * volts.get("vpp", 0.0)
    return swa, swb, swc, swa + swb + swc


# Who made the PMIC and which revision it is. JEP106 pair in R3Ch/R3Dh, and a
# revision byte whose fields are packed rather than BCD.
#
# The revision decode confirms itself: R3Bh reads 0x12 on this bench, which
# gives major ((0x12 >> 4) & 3) + 1 = 2 and minor (0x12 >> 1) & 7 = 1, and
# ZenTimings shows "rev 2.1" for the same part.
VENDOR_BANK_REGISTER = 0x3C
VENDOR_CODE_REGISTER = 0x3D
REVISION_REGISTER = 0x3B

# (JEP106 bank, code) -> name, entered when confirmed against a reference.
PMIC_VENDOR_NAMES = {
    (11, 0x8C): "Richtek Power",
}


def decode_pmic_vendor(bank_byte, code_byte):
    """Return the PMIC maker's name, or its raw JEP106 ID when unlisted."""
    if bank_byte is None or code_byte is None:
        return None
    bank = (int(bank_byte) & 0x7F) + 1
    code = int(code_byte) & 0xFF
    return PMIC_VENDOR_NAMES.get((bank, code), "0x%02X%02X" % (bank_byte, code))


def decode_pmic_revision(raw):
    """Return the PMIC revision as "2.1", or None when it did not read."""
    if raw is None:
        return None
    raw = int(raw) & 0xFF
    return "%d.%d" % (((raw >> 4) & 0x03) + 1, (raw >> 1) & 0x07)


def read_pmic_identity(reader, address, controller=0x00):
    """Return ``{"vendor": ..., "revision": ...}`` for one PMIC."""
    identity = {}
    try:
        bank = reader.read_byte(address, VENDOR_BANK_REGISTER, controller)
        code = reader.read_byte(address, VENDOR_CODE_REGISTER, controller)
        revision = reader.read_byte(address, REVISION_REGISTER, controller)
    except (OSError, TimeoutError, ValueError):
        return identity
    vendor = decode_pmic_vendor(bank, code)
    if vendor:
        identity["vendor"] = vendor
    revision_text = decode_pmic_revision(revision)
    if revision_text:
        identity["revision"] = revision_text
    return identity


def read_adc_millivolts(reader, address, channel, controller=0x00, sleep=None):
    """Select an ADC channel, let it settle, and return the sample in mV."""
    if sleep is None:
        import time

        sleep = time.sleep
    try:
        reader.write_byte(
            address, TELEMETRY_SELECT_REGISTER, select_byte(channel), controller
        )
        sleep(TELEMETRY_SETTLE_SECONDS)
        # First read is discarded; see TELEMETRY_SETTLE_SECONDS.
        reader.read_byte(address, TELEMETRY_VALUE_REGISTER, controller)
        raw = reader.read_byte(address, TELEMETRY_VALUE_REGISTER, controller)
    except (OSError, TimeoutError, ValueError):
        return None
    return decode_adc_mv(channel, raw)


def pmic_address_for_hub(hub_address, hub_addresses=None, pmic_addresses=None):
    """The PMIC that belongs to an SPD hub, by slot position.

    Both devices are addressed by slot: hubs run 0x50-0x57 and PMICs 0x48-0x4F
    in the same order, so slot n answers at 0x50+n and 0x48+n. The bench proves
    the pairing -- hubs 0x51 and 0x53 sit with PMICs 0x49 and 0x4B, which is
    what ZenTimings labels those two DIMMs. The Z790 bench proves it again at
    the other slot positions: hubs 0x50 and 0x52 with PMICs 0x48 and 0x4A.
    """
    if hub_addresses is None or pmic_addresses is None:
        from intel_pch_smbus import PMIC_ADDRESSES, SPD_HUB_ADDRESSES

        hub_addresses = SPD_HUB_ADDRESSES if hub_addresses is None else hub_addresses
        pmic_addresses = PMIC_ADDRESSES if pmic_addresses is None else pmic_addresses

    index = int(hub_address) - hub_addresses[0]
    if 0 <= index < len(pmic_addresses):
        return pmic_addresses[index]
    return None


# The resolved transport, kept rather than re-derived.
#
# detect_current_platform opens a WMI connection and runs three queries, which
# measures about 1.08 s on the Z790 bench -- longer than the telemetry window's
# own one-second poll. Resolving it per poll left that window doing more
# waiting on WMI than reading the bus, on the Tk thread, so the UI stuttered.
# The platform cannot change while the machine is running.
_BACKEND = []


def default_smbus_backend():
    """Return ``(reader factory, controllers, hubs, pmics)`` for this machine.

    The DDR5 devices and their decode are identical on both platforms; the
    host controller they hang off is not. Dispatching here mirrors how
    timings.py selects a timing backend, and keeps the same safety boundary:
    only the selected platform's transport is imported, so an Intel reader is
    never pulled in on AMD hardware or the other way round.

    Resolved once; see _BACKEND.
    """
    if _BACKEND:
        return _BACKEND[0]

    from platform_profiles import (
        LGA1700_DDR4, LGA1700_DDR5, LGA1851, detect_current_platform,
    )

    # timings resolves the platform at import and the app imports it at
    # startup, so inside the viewer this answer already exists and asking WMI
    # again would pay that second twice. Read rather than imported: importing
    # timings from a transport would pull a whole timing backend in behind it,
    # and this module is also used on its own by the research probes, where
    # nothing has resolved it yet.
    import sys

    profile = getattr(sys.modules.get("timings"), "ACTIVE_PLATFORM", None)
    if profile is None:
        profile = detect_current_platform()
    if profile in (LGA1700_DDR4, LGA1700_DDR5, LGA1851):
        from intel_pch_smbus import (
            CONTROLLER_OFFSETS, PMIC_ADDRESSES, SPD_HUB_ADDRESSES,
            PchSmbusReader,
        )

        backend = (PchSmbusReader, CONTROLLER_OFFSETS, SPD_HUB_ADDRESSES,
                   PMIC_ADDRESSES)
    else:
        # Cached too: a machine this project does not support will not become
        # one, and re-asking WMI every poll is the cost this cache exists for.
        backend = None
    _BACKEND.append(backend)
    return backend


def read_dimm_telemetry(reader_factory=None, sleep=None, controllers=None,
                        addresses=None, pmic_addresses=None):
    """Return one telemetry dict per populated DIMM, newest values only.

    Each entry carries the slot's SPD hub temperature and its own PMIC's
    measured rails, so two modules on one board setting report separately --
    which is the point: their VDDQ differs here by 15 mV.

    With no transport given the platform picks one; see
    :func:`default_smbus_backend`. A caller may pass its own instead, which is
    what the tests do.
    """
    from ddr5_pmic import read_dimm_temperatures, spd_hub_channel

    try:
        if reader_factory is None or controllers is None or addresses is None:
            backend = default_smbus_backend()
            if backend is None:
                return []
            default_factory, default_controllers, default_hubs, default_pmics = backend
            reader_factory = reader_factory or default_factory
            controllers = default_controllers if controllers is None else controllers
            addresses = default_hubs if addresses is None else addresses
            pmic_addresses = (
                default_pmics if pmic_addresses is None else pmic_addresses
            )

        reader = reader_factory()
        if not reader.is_driver_open():
            return []

        temperatures = read_dimm_temperatures(
            lambda: reader, controllers, addresses
        )
        modules = []
        for controller in controllers:
            for hub in addresses:
                channel = spd_hub_channel(hub)
                if channel is None:
                    continue
                if not reader.probe_address(hub, controller):
                    continue
                pmic = pmic_address_for_hub(hub, addresses, pmic_addresses)
                if pmic is None or not reader.probe_address(pmic, controller):
                    continue
                entry = {
                    "channel": channel,
                    "hub_address": hub,
                    "pmic_address": pmic,
                    "controller": controller,
                }
                if channel in temperatures:
                    entry["hub_temp_c"] = temperatures[channel]
                entry.update(read_pmic_identity(reader, pmic, controller))
                entry.update(read_pmic_telemetry(reader, pmic, controller, sleep))
                modules.append(entry)
            if modules:
                # DIMMs live on one controller; probing an empty address on
                # the other one blocks for nothing.
                break
        return modules
    except Exception:
        return []


def read_pmic_telemetry(reader, address, controller=0x00, sleep=None):
    """Read one PMIC's measured rails and power.

    Returns a dict of millivolt readings keyed by rail plus ``power_w``, with
    anything that did not convert simply absent. Never raises for a bus that
    will not answer: a DIMM that is not there is not an error.
    """
    from ddr5_pmic import CONFIRMED_PMIC_RAILS, decode_rails

    measured = {}
    for name, channel in (
        ("vdd", ADC_SWA_VDD), ("vddq", ADC_SWB_VDDQ), ("vpp", ADC_SWC_VPP),
        ("vin_bulk", ADC_VIN_BULK),
        ("vout_1v8", ADC_VOUT_1V8), ("vout_1v0", ADC_VOUT_1V0),
    ):
        millivolts = read_adc_millivolts(
            reader, address, channel, controller, sleep
        )
        if millivolts is not None:
            measured[name] = millivolts

    registers = {}
    for register in (
        SWA_TELEMETRY_REGISTER, SWB_TELEMETRY_REGISTER, SWC_TELEMETRY_REGISTER,
        TELEMETRY_MODE_REGISTER, TELEMETRY_TOTAL_REGISTER,
        CURRENT_LIMIT_REGISTER, RAIL_CONFIG_REGISTER,
    ):
        try:
            registers[register] = reader.read_byte(address, register, controller)
        except (OSError, TimeoutError, ValueError):
            continue

    # Power needs a rail voltage. Prefer what the ADC just measured; fall back
    # to the VID setting, which is what the rail was asked for.
    volts = {name: measured[name] / 1000.0
             for name in ("vdd", "vddq", "vpp") if name in measured}
    if len(volts) < 3:
        try:
            configured = decode_rails(
                lambda register: reader.read_byte(address, register, controller),
                CONFIRMED_PMIC_RAILS,
            )
        except Exception:
            configured = {}
        for name, key in (("vdd", "dram_vdd"), ("vddq", "dram_vddq"),
                          ("vpp", "dram_vpp")):
            if name not in volts and key in configured:
                volts[name] = configured[key]

    power = decode_power_watts(registers, volts)
    if power is not None:
        measured["power_w"] = power[3]
        measured["power_swa_w"], measured["power_swb_w"], measured["power_swc_w"] = (
            power[0], power[1], power[2],
        )
    return measured
