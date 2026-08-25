"""Rail definitions and voltage formatting for the Voltages view.

Deliberately free of any hardware-access import so the UI row builder, the
Summary layout and the tests can use it without pulling in ctypes/InpOut.
:mod:`amd_smu_voltages` re-exports everything here alongside the reader.
"""

import math
from dataclasses import dataclass


# Which transport serves a rail. Kept here so the rail definition is the single
# routing authority: moving a rail between transports has twice meant editing
# three modules, and forgetting one failed silently as a blank row.
SOURCE_PM_TABLE = "pmtable"     # SMU PM table, amd_smu_voltages
SOURCE_PMIC = "pmic"            # DDR5 PMIC over SMBus, ddr5_pmic
SOURCE_SUPERIO = "superio"      # board Super I/O over LPC, superio_lpc


@dataclass(frozen=True)
class VoltageRail:
    """One displayable rail plus the range that makes a reading believable."""

    key: str
    label: str
    group: str
    min_volts: float
    max_volts: float
    source: str = SOURCE_PM_TABLE


# Rails render top to bottom in exactly this order, in both the Sensors tab
# and the Summary voltage column. Order is the one requested for the AM5 view:
# core first, then SoC, fabric, memory I/O, the DIMM rails, and misc last.
RAILS = (
    VoltageRail("vddcr_vdd", "VDDCR_VDD", "Core", 0.40, 1.55),
    VoltageRail("vddcr_soc", "VDDCR_SOC", "SoC", 0.70, 1.40),
    VoltageRail("cldo_vddq", "VDDP", "Fabric", 0.60, 1.35),
    VoltageRail("vddg_ccd", "VDDG CCD", "Fabric", 0.60, 1.35),
    VoltageRail("vddg_iod", "VDDG IOD", "Fabric", 0.60, 1.35),
    # VDDIO and VTT are board sensor rails: proven absent from both the PM
    # table and the DDR5 PMIC, so they come from the Super I/O.
    VoltageRail("vddio_mem", "VDDIO", "Memory", 0.90, 1.80, SOURCE_SUPERIO),
    VoltageRail("vtt", "VTT", "Memory", 1.50, 2.10, SOURCE_SUPERIO),
    # DRAM VDD/VDDQ/VPP live on the DIMM's own PMIC; the SMU PM table does not
    # carry them at all — see amd_smu_voltages.CONFIRMED_VOLTAGE_OFFSETS.
    VoltageRail("dram_vdd", "DRAM VDD", "Memory", 0.90, 1.65, SOURCE_PMIC),
    VoltageRail("dram_vddq", "DRAM VDDQ", "Memory", 0.90, 1.65, SOURCE_PMIC),
    # VPP is the 1.8 V nominal wordline rail, hence the higher band.
    VoltageRail("dram_vpp", "DRAM VPP", "Memory", 1.50, 2.10, SOURCE_PMIC),
    # Named as HWiNFO does, which reads the same SVI3 telemetry: its
    # "CPU VDD_MISC Voltage (SVI3 TFN)" is this rail, to the millivolt.
    VoltageRail("cdd_misc", "VDD_MISC", "Misc", 0.40, 1.35),
)

RAILS_BY_KEY = {rail.key: rail for rail in RAILS}

# Rails held back from a set-wide voltage list.
SUMMARY_HIDDEN_RAILS = frozenset({"vtt"})

# Rails each module reports for itself. One board setting drives them, but the
# modules do not have to agree: the two DIMMs on this bench sit 15 mV apart on
# VDDQ. A single set-wide row can only show one of those, so these are left to
# the per-DIMM panels in the Sensor Telemetry window, which read each module's
# own PMIC and say which module they came from.
PER_MODULE_RAILS = frozenset({"dram_vdd", "dram_vddq", "dram_vpp"})

# Widest plausible band for any rail on this platform; used by the probe to
# shortlist candidate offsets before a human identifies them. The upper bound
# has to clear DRAM VPP at 1.8 V nominal, so it sits well above the logic rails.
CANDIDATE_MIN_VOLTS = 0.40
CANDIDATE_MAX_VOLTS = 2.10


def validate_voltage(rail, volts):
    """Return the rail voltage, or raise ValueError when it is not believable."""
    volts = float(volts)
    if not math.isfinite(volts):
        raise ValueError("%s is not finite" % rail.label)
    if not (rail.min_volts <= volts <= rail.max_volts):
        raise ValueError(
            "%s %.4f V is outside %.2f-%.2f V"
            % (rail.label, volts, rail.min_volts, rail.max_volts)
        )
    return volts


def format_volts(volts):
    """Render a rail voltage the way memory tuners read it (millivolt step)."""
    return "%.3f V" % float(volts)


def is_candidate_voltage(volts):
    """True when a decoded float could plausibly be any rail on this platform."""
    try:
        volts = float(volts)
    except (TypeError, ValueError):
        return False
    if not math.isfinite(volts):
        return False
    return CANDIDATE_MIN_VOLTS <= volts <= CANDIDATE_MAX_VOLTS
