"""Dump the Python AM5 table under stubs, for the Rust parity test.

The Python it builds is the 1.1.0 release, which the Rust replaced; it is
no longer on this branch. To regenerate the fixture, check out the v1.1.0
tag beside this repository and run this from that checkout's root:

    python <this repository>/crates/rochviewer/src/amd/profile/parity/dump_python_profile.py

It builds rochviewer/amd/profile.py's table three times with nothing real
underneath -- WMI, the SMN reader, the APOB reader, every live source and
every module-level reader the profile calls are stubbed -- reads every row
the way the window does, and writes am5_profile_parity.json beside this
file. profile/parity.rs builds the Rust table under the same stubs and
compares the two row by row: order, names, labels, tab, category, column,
flags, the dual sides, and the text each one displays.

No hardware is touched: a stub that was forgotten would reach the driver,
so every source is either stubbed or fed a fresh empty value.
"""

import json
import os
import struct
import sys
import time
import types
from unittest import mock

# The v1.1.0 checkout this is run from, which holds the rochviewer package.
ROOT = os.getcwd()
sys.path.insert(0, ROOT)
OUT = os.path.join(os.path.dirname(__file__), "am5_profile_parity.json")


# -- WMI ---------------------------------------------------------------------

def wmi_classes(scenario):
    if scenario == "failing":
        return {}
    return {
        "Win32_Processor": [dict(
            ProcessorId="178BFBFF00B40F40", ExtClock=100,
            Name=CPU_NAMES[scenario], Manufacturer="AuthenticAMD",
            NumberOfCores=8, NumberOfLogicalProcessors=16,
            SocketDesignation="AM5", L3CacheSize=98304,
        )],
        "Win32_BaseBoard": [dict(
            Product="X870 AORUS ELITE WIFI7", Version="x.x",
            Manufacturer="Gigabyte Technology Co., Ltd.",
        )],
        "Win32_BIOS": [dict(SMBIOSBIOSVersion=" F5 ",
                            ReleaseDate="20260315000000.000000+000")],
        "Win32_PhysicalMemory": [
            dict(Capacity="34359738368", PartNumber="F5-6000J3038F16G",
                 Manufacturer="G Skill Intl", DeviceLocator="DIMM 1",
                 BankLabel="P0 CHANNEL A", SerialNumber="00000000",
                 Attributes=2, SMBIOSMemoryType=34),
            dict(Capacity="34359738368", PartNumber="F5-6000J3038F16G",
                 Manufacturer="G Skill Intl", DeviceLocator="DIMM 1",
                 BankLabel="P0 CHANNEL B", SerialNumber="00000000",
                 Attributes=2, SMBIOSMemoryType=34),
        ],
        "Win32_PhysicalMemoryArray": [dict(MemoryDevices=4)],
    }


class FakeConnection:
    """Answers for whichever scenario is current when a class is queried,
    so a connection the profile kept from an earlier scenario cannot leak
    that scenario's machine into the next."""

    def __getattr__(self, name):
        if not name.startswith("Win32_"):
            raise AttributeError(name)
        classes = wmi_classes(CURRENT["scenario"])
        return lambda: [types.SimpleNamespace(**fields)
                        for fields in classes.get(name, [])]


CURRENT = {"scenario": "granite"}
sys.modules["wmi"] = types.SimpleNamespace(WMI=FakeConnection)


# -- the rest of the inputs --------------------------------------------------

CPU_NAMES = {
    "granite": "AMD Ryzen 7 9850X3D 8-Core Processor",
    "raphael": "AMD Ryzen 7 7800X3D 8-Core Processor",
    "failing": "AMD Ryzen 7 8700G w/ Radeon 780M Graphics",
}


def oracle_regs():
    from tests.test_am5_timings import _oracle_regs

    return _oracle_regs()


def two_channel_regs():
    """UMC0 is the oracle; UMC1 differs where a row would show it."""
    regs = {}
    base = oracle_regs()
    base[0x502A4] = 4 | (3 << 8) | (1 << 4) | (1 << 12)     # preambles
    base[0x50050] = 0x87654321                               # BGS off
    base[0x50058] = 0x87654321
    base[0x500D0] = 0x10                                     # BGS Alt on
    base[0x500D4] = 0
    base[0x50100] = 1 << 12                                  # ECC on
    base[0x50258] = (6 << 24) | (35 << 16) | (21 << 8)
    second = dict(base)
    second[0x50204] = 38 | (58 << 8) | (46 << 16) | (8 << 24)  # tCL 38
    second[0x5012C] = 0x0539114A                             # Mixed refresh
    second[0x502A4] = 1 | (3 << 8) | (2 << 4) | (1 << 12)    # 2 tCK, reserved
    second[0x50100] = 0                                      # ECC off
    second[0x50258] = (7 << 24) | (36 << 16) | (22 << 8)
    second[0x50200] = 4100 | (1 << 18)                       # GDM on
    for offset, value in base.items():
        regs[offset] = value
    for offset, value in second.items():
        regs[0x100000 + offset] = value
    return regs


def ccdl_table(run=(22, 84, 44)):
    marker = struct.pack("<2H", 0x5000, 0x00C3)
    chunk = bytearray(b"\x11" * (9 * 2 + 8))
    chunk[0:4] = marker
    struct.pack_into("<3H", chunk, 9 * 2, *run)
    return bytes(64) + bytes(chunk) + bytes(64) + bytes(chunk) + bytes(64)


TRAINING_A = {
    "rtt_wr": "40 RZQ/6", "rtt_nom_rd": "Off", "rtt_nom_wr": "Off",
    "rtt_park": "48 RZQ/5", "rtt_park_dqs": "40 RZQ/6",
    "ca_odt_a": "480 Ω", "ck_odt_a": "Off", "cs_odt_a": "Off",
    "ca_odt_b": "60 Ω", "ck_odt_b": "60 Ω", "cs_odt_b": "60 Ω",
    "proc_odt_pu": "60 Ω", "proc_odt_pd": "34.3 Ω",
    "proc_ca_ds": "40 Ω", "proc_ck_ds": "60 Ω", "proc_cs_ds": "30 Ω",
    "proc_dq_ds": "34.3 Ω", "proc_dq_ds_pu": "60 Ω", "proc_dq_ds_pd": "34.3 Ω",
    "dram_dq_ds_pu": "34 Ω", "dram_dq_ds_pd": "40 Ω",
    "ALERT_PU": 9, "CA_DRV": 9, "PHY_VREF": 40, "DQ_VREF": 60,
    "CA_VREF": 30, "CS_VREF": 91, "RX_DFE": 0, "TX_DFE": 43,
}
TRAINING_B = dict(TRAINING_A, rtt_wr="60 RZQ/4", ca_odt_a="240 Ω",
                  proc_odt_pu="48 Ω", DQ_VREF=62)
del TRAINING_B["TX_DFE"]
TRAINING_RAPHAEL = {
    "rtt_wr": "40 RZQ/6", "rtt_nom_rd": "Off", "rtt_nom_wr": "Off",
    "rtt_park": "48 RZQ/5", "rtt_park_dqs": "34 RZQ/7",
    "ca_odt_a": "Off", "ck_odt_a": "Off", "cs_odt_a": "480 Ω",
    "ca_odt_b": "60 Ω", "ck_odt_b": "60 Ω", "cs_odt_b": "60 Ω",
    "proc_odt": "40 Ω", "proc_dq_ds": "34.3 Ω", "proc_ca_ds": "40 Ω",
    "dram_dq_ds": "40 Ω",
}


class FakeReader:
    def __init__(self, regs):
        self.regs = regs
        self.last_error = ""

    def read_many(self, addresses):
        return {address: self.regs.get(address, 0) for address in addresses}


class FakeTrainingReader:
    def __init__(self, values, channel_values=None, raw_table=b""):
        self.values = values
        self.channel_values = channel_values or {}
        self.table_address = 0x0A200000
        self.record_address = 0x0A2000D0
        self.channel_record_addresses = {
            "cha": 0x0A2000D0, "chb": 0x0A200100,
        } if channel_values else {}
        self.raw_table = raw_table
        self.last_error = ""

    def read(self):
        return dict(self.values)


GPU_CARD = {
    "device_id": 0x7550, "revision": 0xC0, "revision_text": "C0",
    "board_manufacturer": "Sapphire", "name": "AMD Radeon RX 9070 XT",
    "vbios": "113-EXT114484-100", "memory_size": "16 GB",
    "code_name": "Navi 48", "technology": "4 nm", "cores": 4096,
    "rops": 128, "tmus": 256, "memory_type": "GDDR6",
    "bus_width": "256 bit", "resizable_bar": "Enabled (16 GB)",
    "pcie_link": "PCIe 5.0 x16 @ 5.0 x2",
    "driver_version": "32.0.21001.9024", "driver_date": "2026-01-15",
}

DIMM_TELEMETRY = [
    {"channel": "a", "vdd": 1425, "vddq": 1410, "vpp": 1800,
     "vin_bulk": 5040, "vout_1v8": 1795, "vout_1v0": 1000},
    {"channel": "b", "vdd": 1410, "vddq": 1395},
]

CORE_CLOCKS = {0: "5012 MHz", 1: None, 2: "4790 MHz"}
AGGREGATE_CLOCKS = {"core_avg": "4712 MHz", "core_effective": "803 MHz"}
BRIDGES = {(0x00, 0): (0x14D8, 0x00), (0x14, 3): (0x790E, 0x51)}


def stub_sources(runtime, scenario):
    from rochviewer.amd.smu_clocks import SmuClocks
    from rochviewer.amd.smu_power import SmuPower
    from rochviewer.amd.smu_voltages import SmuVoltages

    if scenario == "failing":
        values = {"clocks": None, "voltages": None, "power": None,
                  "dram": {}, "board": {}, "board_monitor": {},
                  "board_temp": {}, "dimm_temp": {}}
    else:
        values = {
            "clocks": SmuClocks(
                version=0x540108 if scenario == "raphael" else 0x620105,
                table_base=0x1000, fclk_mhz=2133.0, uclk_mhz=2050.0,
                mclk_mhz=4100.0, verified=scenario != "raphael"),
            "voltages": SmuVoltages(version=0x620105, table_base=0x1000, values={
                "vddcr_vdd": 1.173, "vddcr_soc": 1.2032, "cldo_vddq": 1.0,
                "vddg_ccd": 0.95, "vddg_iod": 0.9915, "cdd_misc": 1.1,
            }),
            "power": SmuPower(
                version=0x620105, table_base=0x1000,
                values={"ppt": 41.0244, "tdc": 9.6954, "edc": 50.25,
                        "scalar": 1.0},
                limits={"ppt": 162.0, "tdc": 120.0, "edc": 180.0},
                temperatures={"cpu": 41.1, "cpu_limit": 90.0,
                              "iod_average": 38.2, "iod_hotspot": 40.0,
                              "l3": 37.5, "vdd_vrm": 45.0,
                              "soc_vrm": 44.25, "misc_vrm": 41.0},
            ),
            "dram": {"dram_vdd": 1.4, "dram_vddq": 1.39, "dram_vpp": 1.8},
            "board": {"vddio_mem": 1.424, "vtt": 1.8},
            "board_monitor": {"board_vcore": 1.206, "plus12v": 12.096,
                              "plus5v": 5.04, "cpu_fan": 1403.0,
                              "cpu_opt": 3648.0, "pch": 37.0},
            "board_temp": {"cpu": 41.0, "vrm": 32.0, "system": 31.5},
            "dimm_temp": {"a": 38.25, "b": 39.0},
        }
    for name, value in values.items():
        source = runtime._sources[name]
        source.value = value
        source.stamp = time.monotonic()
        if value:
            source.status = source._describe(value)


def runtime_for(scenario):
    from rochviewer.amd.profile import Am5Runtime

    if scenario == "granite":
        training = FakeTrainingReader(
            TRAINING_A, {"cha": TRAINING_A, "chb": TRAINING_B},
            raw_table=ccdl_table())
        reader = FakeReader(two_channel_regs())
    elif scenario == "raphael":
        training = FakeTrainingReader(TRAINING_RAPHAEL)
        reader = FakeReader(oracle_regs())
    else:
        training = None
        reader = FakeReader({})

    def training_factory(layout="zen5"):
        if training is None:
            raise RuntimeError("APOB boom")
        return training

    runtime = Am5Runtime(reader_factory=lambda: reader,
                         training_reader_factory=training_factory,
                         cpu_name_factory=lambda: CPU_NAMES[scenario])
    stub_sources(runtime, scenario)
    return runtime


def patches(scenario):
    failing = scenario == "failing"
    return [
        mock.patch("rochviewer.system_identity.os_name", return_value=None if failing else
                   "Microsoft Windows 11 Professional (x64) 24H2 Build 26100.4061"),
        mock.patch("rochviewer.system_identity.microcode",
                   return_value=None if failing else "0xB404032"),
        mock.patch("rochviewer.system_identity.lpcio_name",
                   return_value=None if failing else "ITE IT8696E"),
        mock.patch("rochviewer.system_identity.pci_device_and_revision",
                   side_effect=lambda device, function: (None, None) if failing
                   else BRIDGES.get((device, function), (None, None))),
        mock.patch("rochviewer.gpu.radeon.read_gpu",
                   return_value={} if failing else dict(GPU_CARD)),
        mock.patch("rochviewer.amd.adl.read_sensors",
                   return_value={} if failing else {"GPU Temperature": "45 °C"}),
        mock.patch("rochviewer.amd.adlx.limit_text",
                   return_value="—" if failing else "304 W"),
        mock.patch("rochviewer.sensors.whea_errors.error_text",
                   return_value="—" if failing else "0"),
        mock.patch("rochviewer.sensors.cpu_clocks.clock_text",
                   side_effect=lambda key: None if failing else AGGREGATE_CLOCKS.get(key)),
        mock.patch("rochviewer.sensors.cpu_clocks.core_labels",
                   return_value=[] if failing else ["CPU 0", "CPU 1", "CPU 2"]),
        mock.patch("rochviewer.sensors.cpu_clocks.core_clock_text",
                   side_effect=lambda index: CORE_CLOCKS.get(index)),
        mock.patch("rochviewer.memory.ddr5_telemetry.read_dimm_telemetry",
                   return_value=[] if failing else DIMM_TELEMETRY),
        mock.patch("rochviewer.memory.ddr5_spd.read_identity", return_value=[]),
        mock.patch("rochviewer.amd.agesa.read_agesa_version_inpout",
                   return_value="" if failing else "ComboAm5PI 1.2.0.3e"),
    ]


def text(value):
    value = value() if callable(value) else value
    return str(value)


EXTRA_KEYS = ("hide_when_blank", "diagnostic", "Parent", "rail_key", "rail_group")


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


def scenario_dump(scenario):
    from rochviewer.amd import profile

    CURRENT["scenario"] = scenario
    clear_caches()
    active = patches(scenario)
    for patch in active:
        patch.start()
    try:
        runtime = runtime_for(scenario)
        rows = [dump_row(row) for row in profile.build_timings(runtime)]
        return {
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
    from rochviewer.amd import profile

    result = {
        "inputs": {
            "two_channel_regs": {"0x%X" % k: v for k, v in sorted(two_channel_regs().items())},
            "oracle_regs": {"0x%X" % k: v for k, v in sorted(oracle_regs().items())},
            "ccdl_table": ccdl_table().hex(),
            "training_a": TRAINING_A,
            "training_b": TRAINING_B,
            "training_raphael": TRAINING_RAPHAEL,
        },
        "training_fields": sorted(profile.TRAINING_FIELDS),
        "system_info_sections": [
            [title, column, list(names)]
            for title, column, names in profile.SYSTEM_INFO_SECTIONS
        ],
        "scenarios": {
            name: scenario_dump(name) for name in ("granite", "raphael", "failing")
        },
    }
    with open(OUT, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(result, handle, ensure_ascii=False, indent=1)
        handle.write("\n")
    print("wrote %s: %s" % (OUT, ", ".join(
        "%s %d rows" % (name, len(item["rows"]))
        for name, item in result["scenarios"].items())))


if __name__ == "__main__":
    main()
