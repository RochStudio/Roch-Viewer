"""Dump the Python AM5 table over many varied inputs, for the Rust parity test.

dump_python_profile.py pins three machines. This one covers what those three
never reach: refresh modes other than Normal, reserved preamble codes, a lone
channel, registers that fail plausibility, the tCCD_L fallbacks, training
records that come back empty or without channel geometry, a transport that
raises, readings that sit on a rounding boundary (x.x5, x.xx5, x.5 cycles),
NaN readings, missing limits, falsy card fields, odd WMI shapes -- and
Ryzen 9000, Ryzen 7000 and neither.

Every input a scenario uses is written into the fixture beside what Python
displayed, so profile/parity.rs builds the Rust table from exactly the same
data rather than from a second hand-written copy of it. Floats travel as
their repr() strings, which both sides parse exactly.

As with the other script, run it from the root of a v1.1.0 checkout:

    python <this repository>/crates/rochviewer/src/amd/profile/parity/dump_python_scenarios.py

No hardware is touched: every reader the profile calls is stubbed.
"""

import json
import os
import sys
import time
import types
from unittest import mock

ROOT = os.getcwd()
sys.path.insert(0, ROOT)
OUT = os.path.join(os.path.dirname(__file__), "am5_profile_scenarios.json")

GRANITE = "AMD Ryzen 7 9850X3D 8-Core Processor"
RAPHAEL = "AMD Ryzen 9 7950X 16-Core Processor"
PHOENIX = "AMD Ryzen 7 8700G w/ Radeon 780M Graphics"


# -- register snapshots ------------------------------------------------------

def oracle():
    from tests.test_am5_timings import _oracle_regs

    return _oracle_regs()


def fgr_lone_regs():
    """UMC0 only, refreshing in Mixed mode, every odd encoding at once."""
    regs = oracle()
    regs[0x50200] = 2000 | (1 << 17)          # MCLK 2000, 2T
    regs[0x5012C] = (1 << 1) | (2 << 16) | (1 << 28)   # per-bank + FGR 2: Mixed; powerdown
    regs[0x50260] = 0x00C00138                 # placeholder, skipped
    regs[0x50264] = 0
    regs[0x50268] = (885 << 16) | 701          # tRFC2 885 -> 442.5 ns
    regs[0x502C0] = 0x800                      # low 11 bits zero: skipped
    regs[0x502C4] = 3                          # tRFCsb 3 -> 1.5 ns
    regs[0x50230] = 65535                      # 32767.5 ns
    regs[0x502A4] = 5 | (2 << 4) | (4 << 8) | (3 << 12)   # every reserved code
    regs[0x50198] = 0xFFFFFFFF                 # failed read: tCCD_L dash
    regs[0x502E0] = 0x3F                       # 63 + 7 = 70: top of range
    regs[0x50050] = 0x87654321                 # BGS register 1 missing
    regs[0x500D0] = 0xFFFFFFFF                 # BGS Alt failed
    regs[0x500D4] = 0
    regs[0x50284] = 3 | (2 << 4) | (3 << 8)
    regs[0x50258] = (7 << 24) | (255 << 16) | (128 << 8)
    regs.pop(0x50100, None)
    return regs


def fgr_regs():
    """FGR mode (no per-bank bit) with tRFCsb absent, both channels."""
    regs = {}
    base = oracle()
    base[0x5012C] = 1 << 16                    # FGR 1
    base[0x502C4] = 0                          # no tRFCsb anywhere
    base[0x50200] = 3001                       # odd MCLK for the ns divisions
    base[0x50268] = (531 << 16) | 777
    base[0x502A4] = 1 | (1 << 4) | (0 << 8) | (1 << 12)
    base[0x50100] = 0xFFFFFFFF                 # ECC register failed
    base[0x50050] = 0x87654321
    base[0x50058] = 0x87654321                 # BGS off
    base[0x500D0] = 0
    base[0x500D4] = 0x7F0                      # BGS Alt on via register 1
    base[0x50198] = (31 << 3)                  # 36: top of range
    base[0x502E0] = 0                          # 7: under range
    second = dict(base)
    second[0x5012C] = (1 << 1)                 # Per-Bank Only
    second[0x502A4] = 2 | (3 << 8)             # 2 tCK (1110), 4 tCK write
    second[0x50100] = 1 << 12
    second[0x50200] = 3001 | (1 << 18)
    second[0x50230] = 12345
    second[0x50284] = 0
    for offset, value in base.items():
        regs[offset] = value
    for offset, value in second.items():
        regs[0x100000 + offset] = value
    return regs


def implausible_second_regs():
    """UMC0 good, UMC1 saturated: the lone-channel path from the other side."""
    regs = dict(oracle())
    bad = dict(oracle())
    bad[0x50204] = 0x3F | (0x7F << 8)
    for offset, value in bad.items():
        regs[0x100000 + offset] = value
    regs[0x50198] = 0
    regs[0x502E0] = 0x1B                       # 34
    return regs


def anchor_regs():
    """Registers that anchor find_ccdl_wr: tCCD_L 16, tCCD_L_WR2 34."""
    regs = dict(oracle())
    regs[0x50198] = (11 << 3)                  # 16
    regs[0x502E0] = 27                         # 34
    return regs


# -- APOB tables -------------------------------------------------------------

def table_with_run(run, copies=2, marker=(0x5000, 0x00C3)):
    import struct

    out = bytearray(64)
    for _ in range(copies):
        chunk = bytearray(b"\x11" * (9 * 2 + 8))
        chunk[0:4] = struct.pack("<2H", *marker)
        struct.pack_into("<3H", chunk, 9 * 2, *run)
        out += chunk + bytes(64)
    return bytes(out)


def anchored_table(tccdl=16, wr=40, wr2=34):
    import struct

    return bytes(30) + struct.pack("<3H", tccdl, wr, wr2) + bytes(30)


TRAINING_A = {
    "rtt_wr": "40 RZQ/6", "rtt_nom_rd": "Off", "rtt_nom_wr": "34 RZQ/7",
    "rtt_park": "48 RZQ/5", "rtt_park_dqs": "Off",
    "ca_odt_a": "480 Ω", "ck_odt_a": "Off", "cs_odt_a": "80 Ω",
    "ca_odt_b": "60 Ω", "ck_odt_b": "40 Ω", "cs_odt_b": "60 Ω",
    "proc_odt_pu": "60 Ω", "proc_odt_pd": "34.3 Ω",
    "proc_ca_ds": "40 Ω", "proc_ck_ds": "60 Ω", "proc_cs_ds": "30 Ω",
    "proc_dq_ds": "34.3 Ω", "proc_dq_ds_pu": "60 Ω", "proc_dq_ds_pd": "34.3 Ω",
    "dram_dq_ds_pu": "34 Ω", "dram_dq_ds_pd": "40 Ω",
    "ALERT_PU": 0, "CA_DRV": 255, "PHY_VREF": 1, "DQ_VREF": 60,
    "CA_VREF": 30, "CS_VREF": 91, "RX_DFE": 7, "TX_DFE": 43,
}
TRAINING_B = dict(TRAINING_A, rtt_wr="Off", proc_odt_pd="48 Ω", RX_DFE=8)
del TRAINING_B["CS_VREF"]
TRAINING_RAPHAEL = {
    "rtt_wr": "Off", "rtt_nom_rd": "40 RZQ/6", "rtt_nom_wr": "Off",
    "rtt_park": "34 RZQ/7", "rtt_park_dqs": "34 RZQ/7",
    "ca_odt_a": "Off", "ck_odt_a": "Off", "cs_odt_a": "480 Ω",
    "proc_odt": "48 Ω", "proc_dq_ds": "30 Ω", "proc_ca_ds": "40 Ω",
    "dram_dq_ds": "34 Ω",
}


# -- WMI ---------------------------------------------------------------------

def processor(name, **overrides):
    fields = dict(
        ProcessorId="178BFBFF00B40F40", ExtClock=100, Name=name,
        Manufacturer="AuthenticAMD", NumberOfCores=8,
        NumberOfLogicalProcessors=16, SocketDesignation="AM5",
        L3CacheSize=98304,
    )
    fields.update(overrides)
    return {key: value for key, value in fields.items() if value is not ...}


def memory(bank, capacity="17179869184", **overrides):
    fields = dict(Capacity=capacity, PartNumber="F5-6000J3038F16G",
                  Manufacturer="G Skill Intl", DeviceLocator="DIMM 1",
                  BankLabel=bank, SerialNumber="00000000", Attributes=2,
                  SMBIOSMemoryType=34)
    fields.update(overrides)
    return fields


def wmi(cpu, processor_fields=None, board=None, bios=None, memory_rows=None,
        arrays=4):
    return {
        "Win32_Processor": [processor(cpu, **(processor_fields or {}))],
        "Win32_BaseBoard": [board if board is not None else dict(
            Product="X870 AORUS ELITE WIFI7", Version="x.x",
            Manufacturer="Gigabyte Technology Co., Ltd.")],
        "Win32_BIOS": [bios if bios is not None else dict(
            SMBIOSBIOSVersion=" F5 ", ReleaseDate="20260315000000.000000+000")],
        "Win32_PhysicalMemory": memory_rows if memory_rows is not None else [
            memory("P0 CHANNEL A"), memory("P0 CHANNEL B")],
        "Win32_PhysicalMemoryArray": [dict(MemoryDevices=arrays)],
    }


# -- the scenarios -----------------------------------------------------------

def f(value):
    """A float as both sides will parse it."""
    return repr(float(value))


def hexregs(regs):
    return {"0x%X" % k: v for k, v in sorted(regs.items())}


NAN = "nan"

SCENARIOS = {
    # Ryzen 9000, UMC0 alone in Mixed refresh, every reserved encoding, the
    # rounding boundaries everywhere a float is printed.
    "granite_mixed_lone_channel": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE,
                processor_fields=dict(ExtClock=99, L3CacheSize=33280,
                                      NumberOfCores=...),
                board=dict(Product="  ", Version="To Be Filled By O.E.M.",
                           Manufacturer=None),
                bios=dict(SMBIOSBIOSVersion="1.2.3", ReleaseDate="2026"),
                memory_rows=[memory("P0 CHANNEL A", capacity="25769803776")]),
        regs=hexregs(fgr_lone_regs()),
        training=dict(values=TRAINING_A, channel_values={"cha": TRAINING_A},
                      raw_table=bytes(range(256)).hex(), last_error="",
                      addresses={}),
        sources=dict(
            clocks=dict(version=0x620105, table_base=0x1000,
                        fclk_mhz=f(2133.5), uclk_mhz=f(2132.5),
                        mclk_mhz=f(2000.0), verified=False),
            voltages=dict(version=0x620105, table_base=0x1000, values={
                "vddcr_vdd": f(1.0005), "vddcr_soc": f(1.2345),
                "cldo_vddq": f(0.9995), "vddg_ccd": f(1.0625),
                "vddg_iod": f(0.8125)}),
            power=dict(version=0x620105, table_base=0x1000,
                       values={"ppt": f(41.25), "tdc": f(9.95),
                               "edc": f(50.05), "scalar": f(1.005)},
                       limits={"ppt": f(162.25), "tdc": f(120.35)},
                       temperatures={"cpu": f(41.25), "iod_average": f(38.15),
                                     "iod_hotspot": f(40.05), "l3": f(0.25),
                                     "vdd_vrm": f(45.75), "misc_vrm": f(-1.25)}),
            dram={"dram_vdd": f(1.4005)},
            board={"vddio_mem": f(1.2125)},
            board_monitor={"board_vcore": f(1.0625), "plus12v": f(12.0965),
                           "cpu_fan": f(1403.9), "cpu_opt": f(0.0),
                           "pciex16": f(55.25)},
            board_temp={"cpu": f(41.25), "system": f(31.05)},
            dimm_temp={"a": f(38.25)},
        ),
        seams=dict(
            os_name="Windows 11 Pro 24H2", microcode=None, lpcio="",
            bridges=[[0x00, 0, 0x14D8, 0x0A], [0x14, 3, 0x1234, 0x51]],
            gpu=dict(device_id=0x7550, revision=0, revision_text="",
                     board_manufacturer="", name=None, vbios="113-X",
                     memory_size="", code_name="Navi 48", technology=None,
                     cores=None, rops=64, tmus=None, memory_type="GDDR6",
                     bus_width=None, resizable_bar="Disabled",
                     pcie_link=None, driver_version="", driver_date=None),
            gpu_sensors={"GPU Hotspot": "61 °C"},
            board_power_limit="—", whea="3",
            clock_text={"core_avg": "", "core_effective": "0 MHz"},
            core_labels=["CPU 0", "CPU 1"],
            core_clocks={"0": "", "1": "4100 MHz"},
            dimm_telemetry=[
                {"channel": "a", "vdd": 1425},
                {"channel": "a", "vdd": 1430},
                {"channel": "b", "vdd": 1412, "vddq": 1395, "vpp": 1805,
                 "vin_bulk": 4995, "vout_1v8": 1795, "vout_1v0": 1005},
            ],
            agesa="   ",
        ),
    ),
    # Ryzen 9000 with both channels decoded but disagreeing on everything a
    # setting row shows; the APOB run is ambiguous, so tCCD_L falls back to
    # the registers, one in range and one not.
    "granite_fgr_two_channels": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE,
                processor_fields=dict(ExtClock="99.8", L3CacheSize="65536",
                                      ProcessorId="178BFBFF00A60F12"),
                board=dict(Product="B650 TOMAHAWK", Version=" 1.0 ",
                           Manufacturer="Micro-Star International Co., Ltd."),
                bios=dict(SMBIOSBIOSVersion="7D75v1A", ReleaseDate=None)),
        regs=hexregs(fgr_regs()),
        training=dict(values=TRAINING_A,
                      channel_values={"cha": TRAINING_A, "chb": TRAINING_B},
                      raw_table=(table_with_run((22, 84, 44), copies=1)
                                 + table_with_run((21, 84, 44), copies=1)).hex(),
                      last_error="",
                      addresses={"cha": 0x0A2000D0, "chb": 0x0A200100}),
        sources=dict(
            clocks=dict(version=0x620105, table_base=0xDF000000,
                        fclk_mhz=f(2000.0), uclk_mhz=f(1500.5),
                        mclk_mhz=f(3001.0), verified=True),
            voltages=dict(version=0x620105, table_base=0xDF000000, values={}),
            power=dict(version=0x620105, table_base=0xDF000000,
                       values={"ppt": f(1.05), "tdc": f(0.15)},
                       limits={"ppt": f(88.0), "tdc": f(75.0), "edc": f(150.0),
                               "scalar": f(10.0)},
                       temperatures={"cpu": NAN, "cpu_limit": f(95.0),
                                     "iod_average": NAN, "l3": f(1e20),
                                     "soc_vrm": f("inf")}),
            dram={}, board={},
            board_monitor={"plus5v": NAN, "cpu_fan": f(-3.7),
                           "pch": NAN},
            board_temp={"vrm": NAN},
            dimm_temp={"a": f(38.25), "b": f(39.05)},
        ),
        seams=dict(
            os_name="Microsoft Windows 11 Home (X86)   23H2 Build 22631",
            microcode="0xA601203", lpcio="Nuvoton NCT6799D",
            bridges=[[0x00, 0, 0x14E8, 0x00], [0x14, 3, 0x790E, 0x71]],
            gpu=None,
            gpu_sensors={},
            board_power_limit="250 W", whea="0",
            clock_text={},
            core_labels=[],
            core_clocks={},
            dimm_telemetry=[],
            agesa="ComboAm5PI 1.1.0.2c",
        ),
    ),
    # Ryzen 7000 on two channels with an APOB whose record comes back empty
    # and says why; the run is still found in the table it left behind.
    "raphael_empty_record": dict(
        cpu=RAPHAEL,
        wmi=wmi(RAPHAEL, processor_fields=dict(ExtClock=0)),
        regs=hexregs(fgr_regs()),
        training=dict(values={}, channel_values={}, last_error=
                      "APOB training block not found",
                      raw_table=table_with_run((16, 64, 32), copies=3).hex(),
                      addresses={}),
        sources=dict(
            clocks=dict(version=0x540108, table_base=0x2000,
                        fclk_mhz=f(1800.0), uclk_mhz=f(3001.0),
                        mclk_mhz=f(3001.0), verified=False),
            voltages=None, power=None,
            dram={}, board={}, board_monitor={}, board_temp={}, dimm_temp={},
        ),
        seams=dict(
            os_name=None, microcode="0xA601206", lpcio=None,
            bridges=[], gpu=None, gpu_sensors={}, board_power_limit="—",
            whea="—", clock_text={"core_avg": "5100 MHz"},
            core_labels=["CPU 0"], core_clocks={}, dimm_telemetry=[],
            agesa="",
        ),
    ),
    # Ryzen 7000 with its one record, UMC1 saturated, clocks not read.
    "raphael_single_record": dict(
        cpu=RAPHAEL,
        wmi=wmi(RAPHAEL, processor_fields=dict(ExtClock=None)),
        regs=hexregs(implausible_second_regs()),
        training=dict(values=TRAINING_RAPHAEL, channel_values={},
                      last_error="", raw_table="", addresses={}),
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name="Microsoft Windows 10 Pro (x64)", microcode=None,
            lpcio=None, bridges=[[0x00, 0, 0x14D8, 0xFF]], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—",
            clock_text={}, core_labels=[], core_clocks={},
            dimm_telemetry=[], agesa="ComboAm5PI 1.0.0.7b",
        ),
    ),
    # The SMN transport cannot be opened, and the APOB names the missing
    # driver: the status collapses to the driver problem.
    "granite_smn_raises": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE),
        regs_raise="MCFG base not found",
        training=dict(values={}, channel_values={},
                      last_error="inpoutx64.dll not found. Looked in:\nC:\\x",
                      raw_table=table_with_run((21, 83, 42)).hex(),
                      addresses={}),
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name=None, microcode=None, lpcio=None, bridges=[], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—", clock_text={},
            core_labels=[], core_clocks={}, dimm_telemetry=[], agesa="",
        ),
    ),
    # Ryzen 9000 whose APOB reader cannot be opened at all, with a run that
    # only the register anchor could have found -- but no table to find it in.
    "granite_apob_raises": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE, processor_fields=dict(Name="  ")),
        regs=hexregs(anchor_regs()),
        training_raise="APOB mapping failed",
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name=None, microcode=None, lpcio=None, bridges=[], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—", clock_text={},
            core_labels=[], core_clocks={}, dimm_telemetry=[], agesa="",
        ),
    ),
    # The register anchor finds tCCD_L_WR where no marker does.
    "granite_anchored_ccdl": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE),
        regs=hexregs(anchor_regs()),
        training=dict(values={"rtt_wr": "Off"}, channel_values={},
                      last_error="", raw_table=anchored_table().hex(),
                      addresses={}),
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name=None, microcode=None, lpcio=None, bridges=[], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—", clock_text={},
            core_labels=[], core_clocks={}, dimm_telemetry=[], agesa="",
        ),
    ),
    # Firmware strings padded with the information separators -,
    # which str.strip() removes and Rust's trim() does not; and a register
    # snapshot in which no channel is plausible, so the reader's own error
    # rides on the status.
    "granite_separators_nothing_plausible": dict(
        cpu=GRANITE,
        wmi=wmi(GRANITE,
                processor_fields=dict(Name="AMD Ryzen 7 9850X3D",
                                      Manufacturer="AuthenticAMD",
                                      SocketDesignation="AM5"),
                board=dict(Product="X870E HERO", Version="x.x",
                           Manufacturer="ASUSTeK COMPUTER INC."),
                bios=dict(SMBIOSBIOSVersion="1001",
                          ReleaseDate="20260101000000.000000+000")),
        regs=hexregs({0x50200: 4100, 0x50204: 0, 0x100000 + 0x50200: 0xFFFF}),
        training=dict(values={}, channel_values={}, last_error="",
                      raw_table="", addresses={}),
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name="Microsoft Windows 11 Pro (x64) 24H2",
            microcode=None, lpcio=None, bridges=[], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—", clock_text={},
            core_labels=[], core_clocks={}, dimm_telemetry=[],
            agesa="ComboAm5PI 1.2.0.3e",
        ),
    ),
    # A processor this profile does not train for, on good registers.
    "phoenix_no_training": dict(
        cpu=PHOENIX,
        wmi=wmi(PHOENIX, processor_fields=dict(ProcessorId="xyz")),
        regs=hexregs(anchor_regs()),
        training=dict(values=TRAINING_A, channel_values={}, last_error="",
                      raw_table=table_with_run((21, 83, 42)).hex(),
                      addresses={}),
        sources=dict(clocks=None, voltages=None, power=None, dram={},
                     board={}, board_monitor={}, board_temp={},
                     dimm_temp={}),
        seams=dict(
            os_name=None, microcode=None, lpcio=None, bridges=[], gpu=None,
            gpu_sensors={}, board_power_limit="—", whea="—", clock_text={},
            core_labels=[], core_clocks={}, dimm_telemetry=[], agesa="",
        ),
    ),
}


# -- building the Python runtime from a scenario -----------------------------

CURRENT = {"wmi": {}}


class FakeConnection:
    def __getattr__(self, name):
        if not name.startswith("Win32_"):
            raise AttributeError(name)
        classes = CURRENT["wmi"] or {}
        if not classes:
            raise RuntimeError("WMI unavailable")
        return lambda: [types.SimpleNamespace(**fields)
                        for fields in classes.get(name, [])]


sys.modules["wmi"] = types.SimpleNamespace(WMI=FakeConnection)


class FakeReader:
    def __init__(self, regs):
        self.regs = regs
        self.last_error = "SMN read refused"

    def read_many(self, addresses):
        return {address: self.regs.get(address) for address in addresses}


class FakeTrainingReader:
    def __init__(self, spec):
        self.values = spec["values"]
        self.channel_values = spec["channel_values"]
        self.channel_record_addresses = spec["addresses"]
        self.raw_table = bytes.fromhex(spec["raw_table"])
        self.last_error = spec["last_error"]
        self.table_address = 0x0A200000
        self.record_address = 0x0A2000D0

    def read(self):
        return dict(self.values)


def floats(mapping):
    return {key: float(value) for key, value in mapping.items()}


def stub_sources(runtime, spec):
    from rochviewer.amd.smu_clocks import SmuClocks
    from rochviewer.amd.smu_power import SmuPower
    from rochviewer.amd.smu_voltages import SmuVoltages

    values = {}
    clocks = spec["clocks"]
    values["clocks"] = None if clocks is None else SmuClocks(
        version=clocks["version"], table_base=clocks["table_base"],
        fclk_mhz=float(clocks["fclk_mhz"]), uclk_mhz=float(clocks["uclk_mhz"]),
        mclk_mhz=float(clocks["mclk_mhz"]), verified=clocks["verified"])
    voltages = spec["voltages"]
    values["voltages"] = None if voltages is None else SmuVoltages(
        version=voltages["version"], table_base=voltages["table_base"],
        values=floats(voltages["values"]))
    power = spec["power"]
    values["power"] = None if power is None else SmuPower(
        version=power["version"], table_base=power["table_base"],
        values=floats(power["values"]), limits=floats(power["limits"]),
        temperatures=floats(power["temperatures"]))
    for name in ("dram", "board", "board_monitor", "board_temp", "dimm_temp"):
        values[name] = floats(spec[name])
    for name, value in values.items():
        source = runtime._sources[name]
        source.value = value
        source.stamp = time.monotonic()
        if value:
            source.status = source._describe(value)


def runtime_for(spec):
    from rochviewer.amd.profile import Am5Runtime

    def reader_factory():
        if "regs_raise" in spec:
            raise RuntimeError(spec["regs_raise"])
        return FakeReader({int(k, 16): v for k, v in spec["regs"].items()})

    def training_factory(layout="zen5"):
        if "training_raise" in spec:
            raise RuntimeError(spec["training_raise"])
        return FakeTrainingReader(spec["training"])

    runtime = Am5Runtime(reader_factory=reader_factory,
                         training_reader_factory=training_factory,
                         cpu_name_factory=lambda: spec["cpu"])
    stub_sources(runtime, spec["sources"])
    return runtime


def patches(seams):
    bridges = {(device, function): (identity, revision)
               for device, function, identity, revision in seams["bridges"]}
    return [
        mock.patch("rochviewer.system_identity.os_name",
                   return_value=seams["os_name"]),
        mock.patch("rochviewer.system_identity.microcode",
                   return_value=seams["microcode"]),
        mock.patch("rochviewer.system_identity.lpcio_name",
                   return_value=seams["lpcio"]),
        mock.patch("rochviewer.system_identity.pci_device_and_revision",
                   side_effect=lambda device, function:
                   bridges.get((device, function), (None, None))),
        mock.patch("rochviewer.gpu.radeon.read_gpu",
                   return_value=None if seams["gpu"] is None
                   else dict(seams["gpu"])),
        mock.patch("rochviewer.amd.adl.read_sensors",
                   return_value=dict(seams["gpu_sensors"])),
        mock.patch("rochviewer.amd.adlx.limit_text",
                   return_value=seams["board_power_limit"]),
        mock.patch("rochviewer.sensors.whea_errors.error_text",
                   return_value=seams["whea"]),
        mock.patch("rochviewer.sensors.cpu_clocks.clock_text",
                   side_effect=lambda key: seams["clock_text"].get(key)),
        mock.patch("rochviewer.sensors.cpu_clocks.core_labels",
                   return_value=list(seams["core_labels"])),
        mock.patch("rochviewer.sensors.cpu_clocks.core_clock_text",
                   side_effect=lambda index: seams["core_clocks"].get(str(index))),
        mock.patch("rochviewer.memory.ddr5_telemetry.read_dimm_telemetry",
                   return_value=[dict(entry) for entry in seams["dimm_telemetry"]]),
        mock.patch("rochviewer.memory.ddr5_spd.read_identity", return_value=[]),
        mock.patch("rochviewer.amd.agesa.read_agesa_version_inpout",
                   return_value=seams["agesa"]),
    ]


EXTRA_KEYS = ("hide_when_blank", "diagnostic", "Parent", "rail_key", "rail_group")


def text(value):
    value = value() if callable(value) else value
    return str(value)


def dump_row(row):
    dual = any(key in row for key in ("value_a", "value_b"))
    out = {
        "name": row["name"],
        "display_name": row.get("display_name"),
        "tab": row["Tab"],
        "category": row["Category"],
        "column": row["Column"],
        "live": bool(row.get("live")),
        "lone_channel_alone": bool(row.get("lone_channel_alone")),
        "dual": dual,
        "name_a": row.get("name_a"),
        "name_b": row.get("name_b"),
        "refresh": callable(row.get("refresh")),
        "dim": row["dim"]() if callable(row.get("dim")) else None,
        "extra": {key: row[key] for key in EXTRA_KEYS if key in row},
        "value": text(row["value"]),
    }
    if dual:
        out["value_a"] = text(row["value_a"])
        out["value_b"] = text(row["value_b"])
    return out


def clear_caches():
    from rochviewer.amd import profile
    from rochviewer.memory import dimm_inventory

    for name in ("_SYSTEM_INFO_CACHE", "_PROCESSOR_FACTS", "_GPU_CACHE",
                 "_GPU_SENSOR_CACHE", "_WMI_CONNECTION"):
        getattr(profile, name).clear()
    dimm_inventory._CACHE.clear()


def scenario_dump(spec):
    from rochviewer.amd import profile

    CURRENT["wmi"] = spec["wmi"]
    clear_caches()
    active = patches(spec["seams"])
    for patch in active:
        patch.start()
    try:
        runtime = runtime_for(spec)
        rows = [dump_row(row) for row in profile.build_timings(runtime)]
        return {
            "inputs": spec,
            "rows": rows,
            "status": runtime.status,
            "training_status": runtime.training_status,
            "clock_status": runtime.clock_status,
            "voltage_status": runtime.voltage_status,
            "power_status": runtime.power_status,
            "dram_rail_status": runtime.dram_rail_status,
            "board_rail_status": runtime.board_rail_status,
        }
    finally:
        for patch in reversed(active):
            patch.stop()


def main():
    result = {"scenarios": {name: scenario_dump(spec)
                            for name, spec in SCENARIOS.items()}}
    with open(OUT, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(result, handle, ensure_ascii=False, indent=1)
        handle.write("\n")
    print("wrote %s: %s" % (OUT, ", ".join(
        "%s %d rows" % (name, len(item["rows"]))
        for name, item in result["scenarios"].items())))


if __name__ == "__main__":
    main()
