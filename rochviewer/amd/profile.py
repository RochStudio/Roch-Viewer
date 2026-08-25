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

"""AM5 timing profile and lazy read-only runtime for Roch Viewer."""

import re
import time

from rochviewer.amd.smn_mcfg import McfgSmnReader
from rochviewer.amd.apob import (
    GraniteRidgeApobReader,
    find_ccdl_run,
    find_ccdl_wr,
)
from rochviewer.amd.timings import ALL_OFFSETS, UMC_BASES, decode_channel
from rochviewer.platform_profiles import is_granite_ridge_cpu
# Hardware-free leaf modules: safe to import at module level, and doing so
# keeps the import out of the per-read getters that run every refresh.
from rochviewer.amd.power_metrics import METRICS, METRICS_BY_KEY, format_power
from rochviewer.sensors.voltage_rails import (
    PER_MODULE_RAILS,
    RAILS,
    RAILS_BY_KEY,
    SOURCE_PM_TABLE,
    SOURCE_PMIC,
    SOURCE_SUPERIO,
    format_volts,
)

EM_DASH = "—"

# Voltages, power limits and temperatures are all live sensor readings, so they
# share one tab rather than being split by which unit they happen to use.
SENSOR_TAB = "Sensors"

# What training settled on, and what the controller was configured with:
# neither is a timing, and each fills a page of its own.
SKEW_TAB = "Skew"
MISC_TAB = "Misc"

# What the processor was configured to allow, as opposed to what it is
# drawing. The live halves stay on Telemetry, which keeps a maximum and is
# where a limit is worth watching a load against.
LIMITS_CATEGORY = "Limits"

# Privileged reads contend with other monitoring tools for the PCI/SMBus
# mutexes, so a single failure must not blank a row for the whole session.
LAZY_READ_ATTEMPTS = 3

# Voltages, currents and power move constantly, so a successful read is only
# reused briefly. Timings are not covered by this: they cannot change without
# a reboot, and re-reading them would burn privileged transactions for nothing.
LIVE_CACHE_SECONDS = 0.75
TRAINING_FIELDS = frozenset({
    "rtt_nom_wr", "rtt_nom_rd", "rtt_wr", "rtt_park", "rtt_park_dqs",
    "ca_odt_a", "ck_odt_a", "cs_odt_a",
    "ca_odt_b", "ck_odt_b", "cs_odt_b",
    "proc_odt_pu", "proc_odt_pd",
    "proc_ca_ds", "proc_ck_ds", "proc_cs_ds",
    "proc_dq_ds_pu", "proc_dq_ds_pd",
    "dram_dq_ds_pu", "dram_dq_ds_pd",
})


def _current_cpu_name():
    # Through the same cached lookup as the other processor fields, so
    # platform detection and the identity rows share one WMI connection
    # rather than opening a second one that costs another second.
    return _processor_facts().get("name") or ""


def _import_call(module_name, function_name):
    """Call a hardware entry point, imported lazily to keep startup light.

    importlib rather than __import__: for a dotted name __import__ returns
    the top-level package, so once these modules moved into rochviewer.* it
    would have looked the function up on the package and raised. The names
    below are dotted now, which is why this had to change with them.
    """
    import importlib

    return getattr(importlib.import_module(module_name), function_name)()


def _offsets_gate(module_name, table_name, message):
    """Return ``message`` when a transport has no confirmed offsets yet.

    Fail closed: with nothing confirmed for this platform every row stays blank
    rather than showing a guess.
    """
    import importlib

    if getattr(importlib.import_module(module_name), table_name):
        return ""
    return message


class _LiveSource:
    """One lazily-read hardware source: read, cache briefly, keep last good.

    All the privileged reads follow the same policy, so it lives here once:

      * a success is cached for ``ttl`` seconds (``None`` = for the session,
        which is what the clock read wants since clocks cannot change);
      * a failure keeps the previous value, because these reads compete for the
        PCI/SMBus/ISA mutexes with other monitoring tools and losing a race is
        routine — blanking a row for it is not;
      * repeated failures give up after ``LAZY_READ_ATTEMPTS`` so a permanently
        absent transport costs one dict lookup per tick instead of a full
        bus scan.

    ``describe`` turns a successful result into the status string; ``label``
    names the source in the failure strings.
    """

    def __init__(self, read, describe, label, empty=None,
                 ttl=LIVE_CACHE_SECONDS, gate=None):
        self._read = read
        self._describe = describe
        self._label = label
        self._empty = empty
        self._ttl = ttl
        self._gate = gate
        self.value = empty
        self.stamp = None
        self.failures = 0
        self.status = "Not read"

    def _fresh(self):
        if self.stamp is None:
            return False
        return self._ttl is None or (time.monotonic() - self.stamp) < self._ttl

    def get(self):
        if self._fresh():
            return self.value
        # "Ever succeeded" is what separates a transport that is absent from
        # one that merely lost a race; the stamp only tracks freshness.
        ever_read = bool(self.value)
        if not ever_read and self.failures >= LAZY_READ_ATTEMPTS:
            return self.value          # gave up; stop paying for the attempt
        reason = self._gate() if self._gate else ""
        if reason:
            self.failures += 1
            self.status = reason
            return self.value
        try:
            result = self._read()
        except Exception as exc:
            self.failures += 1
            self.status = "%s read failed: %s" % (self._label, exc)
            return self.value
        if result:
            self.value = result
            self.stamp = time.monotonic()
            self.failures = 0
            self.status = self._describe(result)
        else:
            self.failures += 1
            self.status = (
                "%s stale (bus busy)" % self._label if ever_read
                else "%s unavailable" % self._label
            )
        return self.value


class Am5Runtime:
    """Load one AM5 UMC snapshot lazily and cache it for the UI session."""

    def __init__(
        self,
        reader_factory=McfgSmnReader,
        training_reader_factory=GraniteRidgeApobReader,
        cpu_name_factory=_current_cpu_name,
    ):
        self._reader_factory = reader_factory
        self._training_reader_factory = training_reader_factory
        self._cpu_name_factory = cpu_name_factory
        self._cpu_name_cache = None
        self._attempted = False
        self._decoded = None
        self._channels = {}
        self._training_attempted = False
        self._training = None
        self._training_channels = {}
        self._apob_table = b""
        self._ccdl_run_attempted = False
        self._ccdl_run = None
        self.raw = {}
        self.active_base = None
        self.status = "Not read"
        self.training_status = "Not read"
        self._agesa_attempted = False
        self._agesa = None
        # One entry per privileged source; the read/cache/retry policy lives in
        # _LiveSource so it exists once rather than five times.
        self._sources = {
            "clocks": _LiveSource(
                self._read_clocks,
                lambda r: "RSMU PM-table 0x%06X @ 0x%X" % (r.version, r.table_base),
                "FCLK/UCLK",
                ttl=None,          # clocks cannot change; cache for the session
            ),
            "voltages": _LiveSource(
                self._read_voltages,
                lambda r: "RSMU PM-table 0x%06X @ 0x%X — %d rail(s)"
                          % (r.version, r.table_base, len(r.values)),
                "Voltages",
                gate=lambda: _offsets_gate(
                    "rochviewer.amd.smu_voltages", "CONFIRMED_VOLTAGE_OFFSETS",
                    "Voltages unavailable — no confirmed PM-table offset "
                    "(run COLLECT_VOLTAGE_REPORT.bat)",
                ),
            ),
            "power": _LiveSource(
                self._read_power,
                lambda r: "RSMU PM-table 0x%06X @ 0x%X — READ-ONLY"
                          % (r.version, r.table_base),
                "Power limits",
                gate=lambda: _offsets_gate(
                    "rochviewer.amd.smu_power", "CONFIRMED_POWER_OFFSETS",
                    "Power limits unavailable — no confirmed PM-table offset",
                ),
            ),
            "dram": _LiveSource(
                lambda: _import_call("rochviewer.memory.ddr5_pmic", "read_dram_rails"),
                lambda r: "DDR5 PMIC READ-ONLY — %d rail(s)" % len(r),
                "DDR5 PMIC", empty={},
            ),
            "board": _LiveSource(
                lambda: _import_call("rochviewer.sensors.superio_lpc", "read_board_rails"),
                lambda r: "Super I/O READ-ONLY — %d rail(s)" % len(r),
                "Super I/O", empty={},
            ),
            # The board's own thermistors, off the same Super I/O the rails
            # come from. One read serves every row on the tick: three rows
            # each unlocking the configuration window would treble the ISA
            # mutex traffic for one set of numbers.
            "board_temp": _LiveSource(
                lambda: _import_call(
                    "rochviewer.intel.intel_board_sensors", "read_board_temperatures"
                ),
                lambda r: "Super I/O READ-ONLY — %d sensor(s)" % len(r),
                "Board sensors", empty={},
            ),
            "dimm_temp": _LiveSource(
                lambda: _import_call("rochviewer.memory.ddr5_pmic", "read_dimm_temperatures"),
                lambda r: "DDR5 SPD hub READ-ONLY — %s" % ", ".join(
                    "Ch%s %.1f C" % (channel.upper(), celsius)
                    for channel, celsius in sorted(r.items())
                ),
                "DIMM sensor", empty={},
            ),
        }

    # Status strings live on the source objects; expose them under the names
    # the rows and the System Info tab already use.
    clock_status = property(lambda self: self._sources["clocks"].status)
    voltage_status = property(lambda self: self._sources["voltages"].status)
    power_status = property(lambda self: self._sources["power"].status)
    dram_rail_status = property(lambda self: self._sources["dram"].status)
    board_rail_status = property(lambda self: self._sources["board"].status)

    def cpu_name(self):
        """Return the CPU name, queried at most once.

        The underlying lookup is a WMI query. It is cached because the answer
        cannot change while the machine is running, because WMI is slow enough
        to matter when a caller repeats every second, and because WMI needs COM
        initialised on the calling thread -- so a background refresh must not
        be the thing that first triggers it.
        """
        if self._cpu_name_cache is None:
            try:
                self._cpu_name_cache = self._cpu_name_factory() or ""
            except Exception:
                self._cpu_name_cache = ""
        return self._cpu_name_cache

    def _load(self):
        if self._attempted:
            return
        self._attempted = True
        try:
            reader = self._reader_factory()
            addresses = tuple(
                base + offset for base in UMC_BASES for offset in ALL_OFFSETS
            )
            values = reader.read_many(addresses)
            last_error = ""
            channels = {}
            primary_regs = None
            primary_base = None
            for base in UMC_BASES:
                regs = {}
                for offset in ALL_OFFSETS:
                    value = values.get(base + offset)
                    if value is None:
                        last_error = getattr(reader, "last_error", "") or last_error
                        continue
                    regs[offset] = int(value) & 0xFFFFFFFF
                decoded = decode_channel(regs)
                if decoded is None:
                    continue
                umc_index = base // 0x100000
                channels[umc_index] = decoded
                if primary_base is None:
                    primary_regs = regs
                    primary_base = base
            if channels:
                self._channels = channels
                self._decoded = channels[min(channels)]
                self.raw = primary_regs or {}
                self.active_base = primary_base
                labels = "/".join("UMC%d" % index for index in sorted(channels))
                self.status = "AMD SMN/MCFG READ-ONLY — %s" % labels
                return
            detail = (" (" + last_error + ")") if last_error else ""
            self.status = "AM5 SMN read failed: no plausible UMC channel" + detail
        except Exception as exc:
            self.status = "AM5 SMN read failed: %s" % exc

    def value(self, name):
        if name == "agesa":
            return self.agesa()
        if name in ("fclk_mhz", "uclk_mhz"):
            clocks = self.clocks()
            if clocks is None:
                return EM_DASH
            value = getattr(clocks, name, None)
            return EM_DASH if value is None else value
        if name in TRAINING_FIELDS:
            self._load_training()
            if self._training is None:
                return EM_DASH
            value = self._training.get(name)
            return EM_DASH if value is None else value
        self._load()
        if self._decoded is None:
            return EM_DASH
        value = self._decoded.get(name)
        return EM_DASH if value is None else value

    def ccdl_run(self):
        """``(tCCD_L, tCCD_L_WR, tCCD_L_WR2)`` from the APOB, or None.

        All three, not just the one no register holds. ZenTimings moved the
        whole run out of the UMC because some boards never program 0x50198
        from the BIOS setting, so a register can disagree with what was asked
        for and be doing exactly what the firmware told it. The APOB follows
        the setting.

        The marker route is tried first because it does not need the
        registers to be right. Anchoring on them is kept as the fallback: it
        is what this project used before, and where the registers are correct
        the two agree anyway -- on this bench both give (21, 83, 42).
        """
        if self._ccdl_run_attempted:
            return self._ccdl_run
        self._ccdl_run_attempted = True
        try:
            self._load()
            self._load_training()
            found = find_ccdl_run(self._apob_table)
            if found is None:
                decoded = self._decoded or {}
                tccdl = decoded.get("tCCD_L")
                tccdl_wr2 = decoded.get("tCCD_L_WR2")
                anchored = find_ccdl_wr(self._apob_table, tccdl, tccdl_wr2)
                if anchored is not None:
                    found = (tccdl, anchored, tccdl_wr2)
            self._ccdl_run = found
        except Exception:
            self._ccdl_run = None
        return self._ccdl_run

    def ccdl_value(self, name):
        """One of the three tCCD_L timings, preferring the APOB run.

        Falls back to the register decode, which is what the row showed
        before and is right whenever the firmware did program the register.
        """
        run = self.ccdl_run()
        if run is not None:
            index = ("tCCD_L", "tCCD_L_WR", "tCCD_L_WR2").index(name)
            return run[index]
        if name == "tCCD_L_WR":
            return None
        return (self._decoded or {}).get(name)

    def ccdl_wr(self):
        """tCCD_L_WR, or None: the one tCCD_L timing no UMC register holds."""
        return self.ccdl_value("tCCD_L_WR")

    def agesa(self):
        """Lazy AGESA string from physical-memory marker scan."""
        if self._agesa_attempted:
            return EM_DASH if not self._agesa else self._agesa
        self._agesa_attempted = True
        try:
            from rochviewer.amd.agesa import read_agesa_version_inpout
            version = (read_agesa_version_inpout() or "").strip()
            self._agesa = version or None
        except Exception:
            self._agesa = None
        return EM_DASH if not self._agesa else self._agesa

    def clocks(self):
        """FCLK/UCLK from the approved PM-table version only."""
        return self._sources["clocks"].get()

    def voltages(self):
        """Rail voltages from the approved PM-table version only."""
        return self._sources["voltages"].get()

    def dram_rails(self):
        """DRAM VDD/VDDQ/VPP from the DIMM PMIC over SMBus."""
        return self._sources["dram"].get()

    def board_rails(self):
        """Super I/O rails (CPU VDDIO, VTT). Third transport, see module docs."""
        return self._sources["board"].get()

    def board_temperatures(self):
        """The board's own thermistors, off the same Super I/O."""
        return self._sources["board_temp"].get()

    def power(self):
        """PPT/TDC/EDC from the approved PM-table version only."""
        return self._sources["power"].get()

    def _read_clocks(self):
        from rochviewer.amd.smu_clocks import read_smu_clocks
        self._load()
        umc_mclk = self._decoded.get("mclk_mhz") if self._decoded else None
        return read_smu_clocks(
            cpu_name=self.cpu_name(), umc_mclk_mhz=umc_mclk
        )

    def _read_voltages(self):
        from rochviewer.amd.smu_voltages import read_smu_voltages
        return read_smu_voltages(cpu_name=self.cpu_name())

    def _read_power(self):
        from rochviewer.amd.smu_power import read_smu_power
        return read_smu_power(cpu_name=self.cpu_name())

    def dimm_temperatures(self):
        """Each DIMM's own SPD hub sensor, ``{channel: celsius}``."""
        return self._sources["dimm_temp"].get() or {}

    def voltage_value(self, key):
        """Return one rail in volts, or the em dash when it is unavailable.

        Which transport serves a rail is declared on the rail itself, so a rail
        can be moved between transports by editing its definition alone.
        """
        rail = RAILS_BY_KEY.get(key)
        source = rail.source if rail else SOURCE_PM_TABLE
        if source == SOURCE_PMIC:
            volts = self.dram_rails().get(key)
        elif source == SOURCE_SUPERIO:
            volts = self.board_rails().get(key)
        else:
            result = self.voltages()
            volts = result.values.get(key) if result else None
        return EM_DASH if volts is None else volts

    def channel_umc_value(self, name, channel):
        """Return one UMC-channel timing (cha=UMC0, chb=UMC1) without mirroring."""
        self._load()
        index = 0 if channel == "cha" else 1
        decoded = self._channels.get(index)
        if not decoded:
            return EM_DASH
        value = decoded.get(name)
        return EM_DASH if value is None else value

    def channel_training_value(self, name, channel):
        """Return one exact-geometry-attributed value without mirroring."""
        self._load_training()
        values = self._training_channels.get(channel)
        if not values:
            return EM_DASH
        value = values.get(name)
        return EM_DASH if value is None else value

    def _load_training(self):
        if self._training_attempted:
            return
        self._training_attempted = True
        try:
            if not is_granite_ridge_cpu(self.cpu_name()):
                self.training_status = (
                    "APOB training data disabled — Granite Ridge Ryzen 9000 required"
                )
                return
            reader = self._training_reader_factory()
            values = reader.read()
            # Kept whether or not the training record parsed: tCCD_L_WR is
            # found by scanning the table itself, not the decoded record.
            self._apob_table = getattr(reader, "raw_table", b"") or b""
            if values:
                self._training = dict(values)
                channel_values = getattr(reader, "channel_values", {}) or {}
                if set(channel_values) == {"cha", "chb"}:
                    self._training_channels = {
                        name: dict(item) for name, item in channel_values.items()
                    }
                addresses = getattr(reader, "channel_record_addresses", {}) or {}
                if set(addresses) == {"cha", "chb"}:
                    self.training_status = (
                        "AMD APOB READ-ONLY — table 0x%08X, "
                        "ChA 0x%08X, ChB 0x%08X"
                        % (
                            reader.table_address,
                            addresses["cha"],
                            addresses["chb"],
                        )
                    )
                else:
                    self.training_status = (
                        "AMD APOB READ-ONLY — table 0x%08X, record 0x%08X"
                        % (reader.table_address, reader.record_address)
                    )
                return
            self.training_status = getattr(reader, "last_error", "") or (
                "APOB training data unavailable"
            )
        except Exception as exc:
            self.training_status = "APOB training read failed: %s" % exc


def _format(runtime, name, suffix=""):
    def getter():
        value = runtime.value(name)
        return value if value == EM_DASH else "%s%s" % (value, suffix)

    return getter


def _format_training_channel(runtime, name, channel, suffix=""):
    def getter():
        value = runtime.channel_training_value(name, channel)
        return value if value == EM_DASH else "%s%s" % (value, suffix)

    return getter


def _format_umc_channel(runtime, name, channel, suffix=""):
    def getter():
        value = runtime.channel_umc_value(name, channel)
        return value if value == EM_DASH else "%s%s" % (value, suffix)

    return getter


def _format_umc_channel_decimal(runtime, name, channel, decimals, suffix=""):
    def getter():
        value = runtime.channel_umc_value(name, channel)
        if value == EM_DASH:
            return value
        return ("%%.%df%%s" % decimals) % (float(value), suffix)

    return getter


def _format_decimal(runtime, name, decimals, suffix=""):
    def getter():
        value = runtime.value(name)
        if value == EM_DASH:
            return value
        return ("%%.%df%%s" % decimals) % (float(value), suffix)

    return getter


def _format_voltage(runtime, key):
    def getter():
        volts = runtime.voltage_value(key)
        return volts if volts == EM_DASH else format_volts(volts)

    return getter


def _voltage_status(runtime):
    runtime.voltages()
    return runtime.voltage_status


def _format_power(runtime, key):
    def getter():
        result = runtime.power()
        if result is None:
            return EM_DASH
        text = format_power(
            METRICS_BY_KEY[key],
            result.values.get(key),
            result.limits.get(key),
        )
        return EM_DASH if text is None else text

    return getter


def _power_limit(runtime, key):
    """One power limit on its own, without the live draw beside it.

    Telemetry pairs each limit with what is being drawn against it, because
    that is what a tuner watches while a load runs. Misc wants the other
    half: what the processor was configured to allow, which does not move
    and is not a reading.
    """
    def getter():
        result = runtime.power()
        if result is None:
            return EM_DASH
        text = format_power(METRICS_BY_KEY[key], None,
                            result.limits.get(key))
        return EM_DASH if text is None else text

    return getter


def _thermal_limit_only(runtime):
    """The temperature ceiling alone, without the current reading.

    Read rather than assumed, for the reason _thermal_limit gives: this bench
    reports 90 C where 95 was once taken for granted, so a board that sets it
    differently would be quietly misstated.
    """
    def getter():
        power = runtime.power()
        if power is None:
            return EM_DASH
        limit = power.temperatures.get("cpu_limit")
        return EM_DASH if limit is None else "%.1f °C" % limit

    return getter


def _power_status(runtime):
    runtime.power()
    return runtime.power_status


def _enabled(runtime, name):
    def getter():
        value = runtime.value(name)
        if value == EM_DASH:
            return value
        return "Enabled" if value else "Disabled"

    return getter


def _dram_frequency(runtime):
    value = runtime.value("mclk_mhz")
    return value if value == EM_DASH else "%d MT/s" % (int(value) * 2)


def _nitro(runtime):
    rx = runtime.value("nitro_rx")
    tx = runtime.value("nitro_tx")
    ctrl = runtime.value("nitro_ctrl")
    if EM_DASH in (rx, tx, ctrl):
        return EM_DASH
    return "%s/%s/%s" % (rx, tx, ctrl)


def _status(runtime):
    runtime._load()
    return runtime.status


def _training_status(runtime):
    runtime._load_training()
    return runtime.training_status


# One reading per field, kept for the session. Every field this serves is
# identity or firmware configuration, and none of it can change without a
# reboot. Opening a WMI connection costs about a second, which was invisible
# while these rows were drawn once and never read again -- and became the
# whole telemetry tick the moment Bus Clock joined a window polling at 1 Hz.
_SYSTEM_INFO_CACHE = {}


def _system_info_value(field):
    """Return a lazy WMI value without importing or querying WMI at module load."""
    def getter():
        if field not in _SYSTEM_INFO_CACHE:
            value = _read_system_info(field)
            if value == EM_DASH:
                # Not cached: the transport may simply have been busy, and a
                # blank held for the session would outlive its reason.
                return value
            _SYSTEM_INFO_CACHE[field] = value
        return _SYSTEM_INFO_CACHE[field]

    return getter


_WMI_CONNECTION = []


def _wmi_connection(fresh=False):
    """One WMI connection, opened once and kept.

    Opening one costs about a second. This module was opening a fresh one for
    every field, so the first read of System Info paid a second per row
    instead of a second in total -- nine seconds before the Advanced window,
    which reads every row, could draw.
    """
    if fresh or not _WMI_CONNECTION:
        import wmi

        _WMI_CONNECTION[:] = [wmi.WMI()]
    return _WMI_CONNECTION[0]


def _read_system_info(field):
    """One WMI reading, uncached. See _system_info_value."""
    try:
        return _read_system_info_from(_wmi_connection(), field)
    except Exception:
        pass
    # A kept connection can go stale, and a COM object does not travel
    # between threads. Retrying once on a fresh one tells "this reading is
    # unavailable" apart from "that handle was", which caching a connection
    # would otherwise turn into a permanently blank row.
    try:
        return _read_system_info_from(_wmi_connection(fresh=True), field)
    except Exception:
        return EM_DASH


def _read_system_info_from(connection, field):
    """One WMI reading through an already-open connection."""
    try:
        if field == "cpu":
            name = (_processor_facts().get("name") or "").strip()
            return name or EM_DASH
        if field == "manufacturer":
            maker = (_processor_facts().get("manufacturer") or "").strip()
            return maker or EM_DASH
        if field == "cores":
            facts = _processor_facts()
            cores, threads = facts.get("cores"), facts.get("threads")
            if cores is None or threads is None:
                return EM_DASH
            return "%sC / %sT" % (cores, threads)
        if field == "board":
            boards = connection.Win32_BaseBoard()
            return boards[0].Product.strip() if boards else EM_DASH
        if field == "board_vendor":
            # Reported verbatim; the full legal name is what the board
            # actually publishes and is not abbreviated here.
            boards = connection.Win32_BaseBoard()
            if not boards:
                return EM_DASH
            return (boards[0].Manufacturer or "").strip() or EM_DASH
        if field == "bios":
            bios = connection.Win32_BIOS()
            return bios[0].SMBIOSBIOSVersion.strip() if bios else EM_DASH
        if field == "bios_date":
            # The version string alone does not say how old a build is,
            # and vendors reuse short version numbers across releases.
            bios = connection.Win32_BIOS()
            if not bios:
                return EM_DASH
            raw = str(getattr(bios[0], "ReleaseDate", "") or "")
            # WMI datetime: yyyymmddHHMMSS.ffffff+UUU. Only the date is
            # meaningful for a firmware build.
            if len(raw) < 8 or not raw[:8].isdigit():
                return EM_DASH
            return "%s-%s-%s" % (raw[0:4], raw[4:6], raw[6:8])
        if field == "agesa":
            # Filled by Am5Runtime.agesa() via physical marker scan; keep a
            # direct fallback here for any non-runtime callers.
            try:
                from rochviewer.amd.agesa import read_agesa_version_inpout
                version = (read_agesa_version_inpout() or "").strip()
                return version or EM_DASH
            except Exception:
                return EM_DASH
        if field == "bclk":
            clock = _processor_facts().get("ext_clock")
            if clock in (None, "", 0, "0"):
                return EM_DASH
            return "%s MHz" % clock
        if field == "gpu":
            names = []
            for adapter in connection.Win32_VideoController():
                name = (getattr(adapter, "Name", "") or "").strip()
                # Skip the fallback driver Windows installs before the
                # real one; it is not the card in the machine.
                if name and "Basic Display" not in name and name not in names:
                    names.append(name)
            return " / ".join(names) if names else EM_DASH
        if field == "os":
            systems = connection.Win32_OperatingSystem()
            if not systems:
                return EM_DASH
            caption = (getattr(systems[0], "Caption", "") or "").strip()
            build = (getattr(systems[0], "BuildNumber", "") or "").strip()
            caption = caption.replace("Microsoft ", "")
            return "%s (build %s)" % (caption, build) if build else caption
        if field == "memory":
            total = sum(int(item.Capacity) for item in connection.Win32_PhysicalMemory())
            return "%d GB" % (total // (1024 ** 3)) if total else EM_DASH
    except Exception:
        pass
    return EM_DASH


def _dimm_value(field):
    """Return a lazy per-DIMM value for the System Info rows.

    Size and rank come from SMBIOS Type 17, which is firmware reporting what it
    read from the modules at boot. The DRAM maker and die come from the modules
    themselves over SPD, since SMBIOS does not carry the DRAM component at all.

    Reported for the set of installed modules rather than one slot, since the
    controller settings above these rows are equally set-wide. A mixed kit
    prints each distinct value instead of hiding the mismatch.
    """
    def getter():
        try:
            from rochviewer.memory.dimm_inventory import (
                read_modules, rank_numeric, shared_value, slots_used, split_ic,
            )

            if field == "slots":
                # A board fact rather than a module one: how many sockets
                # there are and how many are filled.
                return slots_used() or EM_DASH

            if field == "part_number":
                return shared_value(
                    read_modules(), lambda module: module.get("part_number")
                )

            if field == "module_manufacturer":
                # Whose name is on the stick, which is not who made the chips
                # on it. The SPD carries it; SMBIOS often says "Unknown",
                # which is a placeholder rather than a name.
                from rochviewer.memory.ddr5_spd import read_identity

                spd = read_identity()
                if spd:
                    named = shared_value(
                        spd, lambda entry: entry.get("module_manufacturer")
                    )
                    if named != EM_DASH:
                        return named
                return shared_value(
                    read_modules(),
                    lambda module: (
                        None
                        if str(module.get("module_manufacturer", "")).strip().lower()
                        in ("", "unknown")
                        else module["module_manufacturer"]
                    ),
                )

            # SPD first: read from the DIMM, not inferred from its SKU. The
            # part-number table only stands in when the SMBus path is shut --
            # no driver, no admin -- and it is a lookup, not a reading.
            if field in ("dram_manufacturer", "dram_die"):
                from rochviewer.memory.ddr5_spd import read_identity

                spd = read_identity()
                if spd:
                    return shared_value(spd, lambda entry: entry[field])
                index = 0 if field == "dram_manufacturer" else 1
                return shared_value(
                    read_modules(),
                    lambda module: split_ic(module["ic"])[index],
                )

            # Read from the modules over SPD, like the maker and die above:
            # SMBIOS carries neither the serial nor the date the module was
            # assembled, and both are per-module facts a mixed kit will
            # disagree about, which shared_value shows rather than hides.
            if field in ("serial_number", "manufacture_date"):
                from rochviewer.memory.ddr5_spd import read_identity

                spd = read_identity()
                if spd:
                    return shared_value(spd, lambda entry: entry.get(field))
                if field == "serial_number":
                    return shared_value(
                        read_modules(),
                        lambda module: module.get("serial_number"),
                    )
                return EM_DASH

            if field == "channels":
                # How many the controller is actually running, counted from
                # what is populated rather than from the socket count: two
                # sticks in one channel is a different machine from one in
                # each, and only the modules say which this is.
                channels = {
                    module.get("channel") for module in (read_modules() or [])
                    if module.get("channel")
                }
                return str(len(channels)) if channels else EM_DASH

            modules = read_modules()
            if field == "size":
                return shared_value(
                    modules,
                    lambda module: (
                        "%d GB" % module["capacity_gb"]
                        if module["capacity_gb"] else EM_DASH
                    ),
                )
            if field == "rank":
                return shared_value(
                    modules, lambda module: rank_numeric(module["rank_count"])
                )
        except Exception:
            pass
        return EM_DASH

    return getter


# What the silicon is called, keyed by what CPUID reports. A decode table is
# not a hardcoded value -- "Granite Ridge" is a name for a code read from the
# hardware, and the entry below is the one this bench answers: family 0x1A,
# model 0x44. The repo's own probe gate names the same pair, independently.
#
# The node is a property of the silicon with nothing on the machine to read it
# from, so it is only reported for a part the table names. An unlisted part
# gets neither a code name nor a node rather than inheriting its neighbour's.
AMD_SILICON = {
    (0x1A, 0x44): ("Granite Ridge", "4 nm"),
}

# 00:00.0 on an AM5 board. The host bridge is the same die as the cores, so
# the name comes from CPUID and only the revision comes from the bridge --
# the same split the Intel side uses for the same reason.
HOST_BRIDGE_DEVICES = {0x14D8: "Granite Ridge"}

# 00:14.3, the FCH's LPC bridge. This is the southbridge in AMD's
# architecture, and it is integrated in the SoC: it answers the same whichever
# discrete chipset the board carries, so it is named FCH rather than "B850".
# The board's marketing chipset name is not something this can read, and
# printing one would be a guess wearing a real revision number.
FCH_DEVICES = {0x790E: "FCH"}


_PROCESSOR_FACTS = []


def _processor_facts():
    """The Win32_Processor fields this profile needs, read once.

    Querying Win32_Processor costs about a second, and it is the query rather
    than the connection: sharing one connection between callers changed
    nothing measurable, while asking once and keeping the answer removed four
    seconds. Eight rows read this class -- CPU, Vendor, Cores / Threads,
    BCLK, Code Name, Technology, Chipset and DRAM Ratio -- and each ran its
    own query, so the Advanced window paid a second per row before drawing.

    The values are copied out rather than the record being kept, so nothing
    here can lazily re-query later or carry a COM object onto another thread.
    A failed read is not cached: the transport may simply have been busy, and
    a blank held for the session would outlive its reason.
    """
    if _PROCESSOR_FACTS:
        return _PROCESSOR_FACTS[0]
    try:
        import wmi

        processors = wmi.WMI().Win32_Processor()
        if not processors:
            return {}
        first = processors[0]
        facts = {
            "processor_id": getattr(first, "ProcessorId", None),
            "ext_clock": getattr(first, "ExtClock", None),
            "name": getattr(first, "Name", None),
            "manufacturer": getattr(first, "Manufacturer", None),
            "cores": getattr(first, "NumberOfCores", None),
            "threads": getattr(first, "NumberOfLogicalProcessors", None),
        }
    except Exception:
        return {}
    _PROCESSOR_FACTS.append(facts)
    return facts


def _cpu_silicon():
    """Return ``(code name, node)`` for this CPU, or (None, None)."""
    try:
        from rochviewer.system_identity import decode_wmi_processor_id

        processor_id = _processor_facts().get("processor_id")
        if not processor_id:
            return None, None
        key = decode_wmi_processor_id(processor_id)
    except Exception:
        return None, None
    return AMD_SILICON.get(key, (None, None))


def _silicon_value(index):
    def getter():
        return _cpu_silicon()[index] or EM_DASH

    return getter


def _chipset():
    """The host bridge: the silicon's code name, and its revision."""
    from rochviewer.system_identity import pci_device_and_revision

    device, revision = pci_device_and_revision(0x00, 0)
    if device is None or device not in HOST_BRIDGE_DEVICES:
        return EM_DASH
    name = _cpu_silicon()[0] or HOST_BRIDGE_DEVICES[device]
    return "AMD %s rev. %02X" % (name, revision)


def _southbridge():
    """The FCH, by the device ID of its LPC bridge, with its revision."""
    from rochviewer.system_identity import pci_device_and_revision

    device, revision = pci_device_and_revision(0x14, 3)
    if device is None or device not in FCH_DEVICES:
        return EM_DASH
    return "AMD %s rev. %02X" % (FCH_DEVICES[device], revision)


def _gpu_value(key):
    """One field of the installed card, read lazily and cached for the run.

    The card cannot change while the machine is on, and eight rows asking
    separately would open the display class key eight times.
    """
    def getter():
        try:
            from rochviewer.gpu import radeon as radeon_gpu

            if not _GPU_CACHE:
                _GPU_CACHE.append(radeon_gpu.read_gpu() or {})
            if key == "rops_tmus":
                return radeon_gpu.rops_tmus_text(lambda: _GPU_CACHE[0]) or EM_DASH
            if key == "cores":
                value = _GPU_CACHE[0].get("cores")
                return EM_DASH if value is None else "%d Unified" % value
            return str(_GPU_CACHE[0].get(key) or EM_DASH)
        except Exception:
            return EM_DASH

    return getter


_GPU_CACHE = []


def _identity(reader):
    """One of the platform-neutral identity readings, read lazily.

    Imported inside the getter so a machine with no WMI, no registry access
    or no Super I/O still builds its rows; the reading blanks, the tab does
    not fail to draw.
    """
    def getter():
        try:
            from rochviewer import system_identity

            return getattr(system_identity, reader)() or EM_DASH
        except Exception:
            return EM_DASH

    return getter()


def _row(name, value, category, tab="Timings", column="Left", **extra):
    """Build one display row.

    ``extra`` carries optional flags the UI reads: ``live=True`` marks a row as
    telemetry that should be re-read while the window is open, and ``rail_key``
    lets the Summary filter on rail identity instead of display label.
    """
    row = {
        "name": name,
        "value": value,
        "Category": category,
        "Tab": tab,
        "Column": column,
    }
    row.update(extra)
    return row


def _uclk_ratio(runtime):
    """Render the UCLK:MCLK ratio, the 1:1 vs 1:2 question.

    Derived rather than read: both clocks are already on the tab, but the
    relationship between them is what actually characterises a memory setup.
    """
    clocks = runtime.clocks()
    mclk = runtime.value("mclk_mhz")
    if clocks is None or mclk == EM_DASH:
        return EM_DASH
    uclk = getattr(clocks, "uclk_mhz", None)
    if not uclk or not mclk:
        return EM_DASH
    ratio = float(mclk) / float(uclk)
    # Only 1:1 and 1:2 exist on this platform; anything else is a bad read.
    if abs(ratio - 1.0) < 0.05:
        return "1:1"
    if abs(ratio - 2.0) < 0.05:
        return "1:2"
    return EM_DASH


def _dram_ratio(runtime):
    """The memory multiplier: how many times BCLK the controller clock is.

    Derived from two rows already on the tab rather than read, because the
    firmware sets AM5 memory speed in MT/s and the multiplier is what that
    works out to. 4100 MHz over a 100 MHz base is 41.
    """
    mclk = runtime.value("mclk_mhz")
    if mclk == EM_DASH:
        return EM_DASH
    try:
        base = float(_processor_facts().get("ext_clock") or 0)
        if not base:
            return EM_DASH
        return "%.2f" % (float(mclk) / base)
    except Exception:
        return EM_DASH


def _temperature(runtime, key):
    def getter():
        power = runtime.power()
        if power is None:
            return EM_DASH
        celsius = power.temperatures.get(key)
        return EM_DASH if celsius is None else "%.1f °C" % celsius

    return getter


def _dimm_temperature(runtime, channel):
    """One module's SPD hub sensor. An empty slot has no hub, so it stays blank."""
    def getter():
        celsius = runtime.dimm_temperatures().get(channel)
        return EM_DASH if celsius is None else "%.1f °C" % celsius

    return getter


def _refresh_is_normal(runtime, channel=None):
    """True when the controller refreshes on tRFC rather than tRFC2."""
    if channel is None:
        mode = runtime.value("refresh_mode")
    else:
        mode = runtime.channel_umc_value("refresh_mode", channel)
    return mode == "Normal"


def _format_refi_ns(runtime, channel=None):
    """Render tREFI as the time it actually is, not the cycle count.

    tREFI is how long the controller may go between refreshes, and a count of
    memory clocks only means something once you know the clock. 65535 cycles
    at 4100 MHz is 15984 ns.

    Whole nanoseconds, matching tRFCns beside it. The two hundredths this
    used to print came from dividing a cycle count by a clock, not from any
    precision the reading has, and a refresh window is not tuned to a
    picosecond.
    """
    def getter():
        cycles = (runtime.value("tREFI") if channel is None
                  else runtime.channel_umc_value("tREFI", channel))
        mclk = runtime.value("mclk_mhz")
        if cycles == EM_DASH or mclk == EM_DASH:
            return EM_DASH
        try:
            megahertz = float(mclk)
            if megahertz <= 0:
                return EM_DASH
            return "%.0f (ns)" % (float(cycles) / megahertz * 1000.0)
        except (TypeError, ValueError):
            return EM_DASH

    return getter


def _format_rfc_ns(runtime, channel=None):
    """Render tRFCns, which means different things per refresh mode.

    In Normal mode the controller refreshes on tRFC, so one interval is shown.
    Otherwise refresh is split between the fine-grain (tRFC2) and same-bank
    (tRFCsb) intervals, so both are shown — the decoder already switches
    tRFC_ns to tRFC2 outside Normal mode.

    The unit is named once, after the values: "117/95 (ns)" rather than
    "117 ns / 95 ns". Both intervals are in the same unit, so saying it twice
    spent a third of the column on repeating it.
    """
    def read(name):
        if channel is None:
            return runtime.value(name)
        return runtime.channel_umc_value(name, channel)

    def getter():
        primary = read("tRFC_ns")
        if primary == EM_DASH:
            return EM_DASH
        if _refresh_is_normal(runtime, channel):
            return "%.0f (ns)" % float(primary)
        same_bank = read("tRFCsb_ns")
        if same_bank == EM_DASH:
            return "%.0f (ns)" % float(primary)
        return "%.0f/%.0f (ns)" % (float(primary), float(same_bank))

    return getter


def _umc_row(runtime, label, category, column, key=None, decimals=None,
             suffix="", dim=None):
    """Build a UMC timing row carrying both channels.

    Every UMC register is decoded per channel, so each timing shows ChA and ChB
    independently rather than mirroring UMC0's value across both.  The shared
    ``value`` stays the primary channel, which is what the Summary column and
    the text dump use.
    """
    key = label if key is None else key
    if decimals is None:
        shared = _format(runtime, key, suffix)
        sides = (
            _format_umc_channel(runtime, key, "cha", suffix),
            _format_umc_channel(runtime, key, "chb", suffix),
        )
    else:
        shared = _format_decimal(runtime, key, decimals, suffix)
        sides = (
            _format_umc_channel_decimal(runtime, key, "cha", decimals, suffix),
            _format_umc_channel_decimal(runtime, key, "chb", decimals, suffix),
        )
    row = _row(label, shared, category, column=column)
    row.update({
        "value_a": sides[0],
        "value_b": sides[1],
        "name_a": "ChA",
        "name_b": "ChB",
    })
    if dim is not None:
        row["dim"] = dim
    return row


def _ccdl_row(runtime, name, category="Tertiary"):
    """One of the three tCCD_L timings, read from the APOB run.

    All three come from the APOB now, not just tCCD_L_WR. The registers are
    kept as the fallback rather than the source: some boards never program
    0x50198 from the BIOS setting, and there a register disagreeing with what
    was asked for is the firmware's doing rather than a misread.

    The run is accepted only when every marker in the table agrees, so both
    channels show the same value rather than an invented per-channel split.
    """
    def value():
        found = runtime.ccdl_value(name)
        return EM_DASH if found is None else found

    row = _row(name, value, category, column="Left")
    row.update({
        "value_a": value,
        "value_b": value,
        "name_a": "ChA",
        "name_b": "ChB",
    })
    return row


def _training_row(runtime, label, key, category, column, tab="Timings"):
    """Build a common row plus separately decoded, geometry-attributed values."""
    row = _row(label, _format(runtime, key), category, tab=tab, column=column)
    row.update({
        "value_a": _format_training_channel(runtime, key, "cha"),
        "value_b": _format_training_channel(runtime, key, "chb"),
        "name_a": "ChA",
        "name_b": "ChB",
    })
    return row


VOLTAGE_CATEGORY = "Voltages"

# Temperatures and the limits they are measured against read as one picture:
# what the chip is doing, and how close that is to what it is allowed to do.
# Thermal Limit is literally a temperature against a limit, so splitting the
# two put a row's two halves either side of a heading.
THERMAL_POWER_CATEGORY = "Thermal & Power"

# Counters that should read zero, and mean something the moment they do not.
ERROR_CATEGORY = "Errors"

# The card's own sensors, which move like the CPU's and belong in the window
# that keeps their extremes rather than on a tab showing one instant.
GRAPHICS_CATEGORY = "Graphics"


def _voltage_rows(runtime):
    """Build the board-wide rail rows, in one section, core outward.

    Each row still carries its supply domain in ``rail_group``, so anything
    that wants to group by domain can; the display simply does not.

    The DRAM rails are not here: they are per-module, and each DIMM's own
    panel reads them from its own PMIC. See PER_MODULE_RAILS.
    """
    return [
        _row(
            rail.label,
            _format_voltage(runtime, rail.key),
            # One section, not one per supply domain. Five headings for eight
            # rails cost more lines than the rails themselves, and RAILS is
            # already ordered core outward, so the domains still read in order
            # without a heading announcing each one.
            VOLTAGE_CATEGORY,
            SENSOR_TAB,
            "Left",
            live=True,
            rail_key=rail.key,
            rail_group=rail.group,
        )
        for rail in RAILS
        if rail.key not in PER_MODULE_RAILS
    ]


def _status_tail(text, prefix, ok="ok"):
    """Shorten one transport's status to what it adds beyond succeeding.

    A message that does not start with the success prefix is a failure, and
    those are passed through whole: the line should grow exactly when it has
    something to say.
    """
    text = str(text or "").strip()
    if not text:
        return EM_DASH
    if not text.startswith(prefix):
        return text
    _label, separator, tail = text.partition("—")
    tail = tail.strip()
    return tail if separator and tail else ok


def _pm_table_segment(status):
    """Compact the PM-table status, keeping the version it was gated on.

    The version is the gate: a BIOS shipping a different one blanks every rail
    and every power reading, so it is the first thing worth seeing.
    """
    status = str(status or "").strip()
    if not status.startswith("RSMU"):
        return status or EM_DASH
    head, separator, tail = status.partition("—")
    version = re.search(r"0x[0-9A-Fa-f]+", head)
    detail = tail.strip().replace("rail(s)", "rails") if separator else ""
    return "PM-table %s (%s)" % (
        version.group(0) if version else "?", detail or "ok"
    )


def _status_summary(runtime):
    """One line for every transport: which answered, and with what.

    Four rows said four versions of "fine" on a working machine. This says it
    once, and the rows themselves stay in the dump for when it is not fine.
    """
    def getter():
        parts = [
            "SMN %s" % _status_tail(_status(runtime), "AMD SMN/MCFG READ-ONLY"),
            # The APOB record addresses stay in the dump; the table address is
            # the part worth carrying here.
            "APOB %s" % _status_tail(
                _training_status(runtime), "AMD APOB READ-ONLY"
            ).split(",")[0],
            _pm_table_segment(_voltage_status(runtime)),
        ]
        power = _status_tail(_power_status(runtime), "RSMU", "ok")
        # The power read is its own sequence and can fail on its own, so it is
        # named even when there is nothing to report but success.
        parts.append("power %s" % ("ok" if power == "READ-ONLY" else power))
        return " · ".join(parts)

    return getter


def _thermal_limit(runtime):
    """Tctl against the limit it is measured against, as the other limits read.

    Both halves are read; neither is assumed. The limit is a setting a BIOS or
    a tuning tool can move -- this bench reports 90.0 C, not the 95 that was
    once assumed -- so a constant here would quietly misstate every board that
    sets it differently.
    """
    def getter():
        power = runtime.power()
        if power is None:
            return EM_DASH
        current = power.temperatures.get("cpu")
        limit = power.temperatures.get("cpu_limit")
        if current is None or limit is None:
            return EM_DASH
        return "%.1f / %.1f °C" % (current, limit)

    return getter


CLOCK_CATEGORY = "Clocks"

# The row the per-processor clocks fold under, and the key that says so.
CORE_EFFECTIVE_CLOCK_ROW = "Core Effective Clock"
PARENT_ROW_KEY = "Parent"


def _core_clock(key):
    """One of the aggregate core clocks, or an em dash if none were read."""
    def getter():
        try:
            from rochviewer.sensors.cpu_clocks import clock_text

            return clock_text(key) or EM_DASH
        except Exception:
            return EM_DASH

    return getter


def _per_core_clock_rows():
    """One row per logical processor, folded under the aggregate row.

    Built from what the counters actually enumerate rather than from a core
    count, so a machine whose counters do not answer gets no children and the
    parent row stands alone.
    """
    try:
        from rochviewer.sensors.cpu_clocks import core_clock_text, core_labels

        labels = core_labels()
    except Exception:
        return []

    def reader(index):
        # core_clock_text returns None before the first interval has been
        # measured, and a row's text goes to the label verbatim: without this
        # every processor would read "None" for the first tick.
        return lambda: core_clock_text(index) or EM_DASH

    return [
        _row(label, reader(index), CLOCK_CATEGORY, SENSOR_TAB, "Left",
             live=True, **{PARENT_ROW_KEY: CORE_EFFECTIVE_CLOCK_ROW})
        for index, label in enumerate(labels)
    ]


def _clock_rows(runtime):
    """The clocks, which a tab can only ever show as one instant.

    What FCLK does across a stability run, and how far the cores fall back
    under load, are exactly the readings worth a minimum and a maximum.

    The core clocks are a different measurement from anything else in this
    project and are named for it. They come from the Windows performance
    counters, which average across the sampling interval rather than
    reporting an instant; see cpu_clocks for the two routes measured and
    rejected before that one.

    MCLK, UCLK, FCLK and DRAM Frequency are not repeated here, though the
    Intel window does carry them. Two reasons, either of them enough. They
    are read once and cached for the session because they cannot change
    without a reboot, so four columns of statistics would be the same number
    four times -- and this window exists for what moves. And row names are
    the key every other view joins on, so a second row called MCLK would
    quietly win the Summary's lookup over the one on System Info.
    """
    return [
        # Fixed too, but kept: it is the reference the core ratios multiply,
        # and one flat row that says what the machine is clocked from is
        # worth its line.
        _row("Bus Clock", _system_info_value("bclk"),
             CLOCK_CATEGORY, SENSOR_TAB, "Left", live=True),
        _row("Core Clock (avg)", _core_clock("core_avg"),
             CLOCK_CATEGORY, SENSOR_TAB, "Left", live=True),
        _row(CORE_EFFECTIVE_CLOCK_ROW, _core_clock("core_effective"),
             CLOCK_CATEGORY, SENSOR_TAB, "Left", live=True),
        # Named as HWiNFO names them, which also keeps them distinct from the
        # System Info rows called FCLK and UCLK -- a row name is the key every
        # other view joins on, so the same name twice would collide.
        _row("Infinity Fabric Clock (FCLK)", _format(runtime, "fclk_mhz", " MHz"),
             CLOCK_CATEGORY, SENSOR_TAB, "Left", live=True),
        _row("Memory Controller Clock (UCLK)",
             _format(runtime, "uclk_mhz", " MHz"),
             CLOCK_CATEGORY, SENSOR_TAB, "Left", live=True),
    ] + _per_core_clock_rows()


# Which Super I/O thermistors this board is confirmed to carry, and what to
# call them. The channel map came from a Z790-P, where channel meanings are a
# board decision rather than a chip one, so every channel was re-checked here
# by loading all sixteen threads and watching what moved:
#
#   cpu     41.0 -> 75.0 -> 43.0, tracking Tctl within 0.5 C the whole way
#   vrm     29.0 -> 32.0 -> 30.5, rising and falling with the load
#   system  31.0 -> 31.5, an ambient sensor behaving like one
#
# Left out, and why:
#
#   pch     exactly 29.0 through a 34 C swing. A chipset that does not care
#           about CPU load and a channel stuck at a constant look identical
#           from here, and nothing distinguishes them without a second board.
#   socket  reads 0.0: the header is not populated on this board.
#
# The board *rails* off the same chip are all left out. Two of the four do
# not decode at all here, and the one labelled vcore reads 1.206 V against a
# real Vcore of 1.173 -- it is following VDDCR_SOC. Wrong names on real
# numbers is the worst thing this tool can do, so none of them ship until
# each is confirmed the way these three were.
BOARD_TEMPERATURES = (
    ("CPU Temp (board)", "cpu"),
    ("VRM Temp (board)", "vrm"),
    ("System Temp", "system"),
)


def _board_temperature(runtime, key):
    def getter():
        celsius = (runtime.board_temperatures() or {}).get(key)
        return EM_DASH if celsius is None else "%.1f °C" % celsius

    return getter


def _board_temperature_rows(runtime):
    """The board's thermistors, beside the ones the CPU reports itself.

    Two paths to the CPU temperature is the point rather than a duplication:
    the board's sensor is what the fan curve actually runs on, and a
    disagreement between it and Tctl is worth being able to see.
    """
    return [
        _row(label, _board_temperature(runtime, key),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True)
        for label, key in BOARD_TEMPERATURES
    ]


_GPU_SENSOR_CACHE = []


def _gpu_sensor(label):
    """One of the card's sensors, from a query shared across the tick.

    ADL returns the whole sensor block in a single call, so all nine rows are
    served by one trip through the driver. The cache is cleared each time the
    first row is asked, which is what makes "once a tick" true rather than
    "once ever".
    """
    def getter():
        try:
            import time

            from rochviewer.amd.adl import read_sensors

            now = time.monotonic()
            if not _GPU_SENSOR_CACHE or now - _GPU_SENSOR_CACHE[0][0] > 0.5:
                _GPU_SENSOR_CACHE[:] = [(now, read_sensors() or {})]
            return _GPU_SENSOR_CACHE[0][1].get(label, EM_DASH)
        except Exception:
            return EM_DASH

    return getter


def _board_power_limit():
    """The ceiling board power is measured against, from ADLX."""
    try:
        from rochviewer.amd.adlx import limit_text

        return limit_text()
    except Exception:
        return EM_DASH


def _graphics_rows():
    """The card's sensors, read through AMD's own driver libraries.

    The readings come from atiadlxx; the limit is the one row atiadlxx has no
    answer for, so it comes from ADLX. It sits directly under board power
    because a power figure means little without the number it is heading for.
    """
    from rochviewer.amd.adl import PMLOG_SENSORS

    rows = [
        _row(label, _gpu_sensor(label), GRAPHICS_CATEGORY, SENSOR_TAB,
             "Right", live=True)
        for _index, label, _unit, _scale in PMLOG_SENSORS
    ]
    rows.append(_row("GPU Board Power Limit", _board_power_limit,
                     GRAPHICS_CATEGORY, SENSOR_TAB, "Right", live=True))
    return rows


def _whea_errors():
    """How many hardware errors Windows has logged since this boot."""
    try:
        from rochviewer.sensors.whea_errors import error_text

        return error_text()
    except Exception:
        return EM_DASH


def _error_rows():
    """The one counter that belongs beside the readings rather than in them.

    A kit that boots and benchmarks can still be quietly correcting errors,
    and this is the only place in the tool that would say so. It sits in the
    window that keeps a maximum, which is the shape that matters here: a
    count that ticked up once during a run is the whole point, and a tab
    showing the current value would lose it.
    """
    return [
        _row("WHEA Errors", _whea_errors, ERROR_CATEGORY, SENSOR_TAB,
             "Right", live=True),
    ]


def _temperature_rows(runtime):
    """Temperature sensors. The DIMM one is the only reading in this project
    that comes from the module itself rather than the CPU or the board."""
    return [
        # Tctl and the limit it is measured against, in one row: the separate
        # "Thermal Limit" row that used to sit below the budgets repeated this
        # temperature verbatim and added only the limit.
        _row("CPU Temp", _thermal_limit(runtime),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        # The DIMM sensors are not here: each module's own panel in the Sensor
        # Telemetry window carries its SPD hub temperature next to the rails
        # from the same PMIC, which is where it belongs.
        # The I/O die, where the memory controller actually lives, and the
        # cache. Both climb far less than the cores, which is what made them
        # hard to tell apart from the die readings at idle and easy under
        # load.
        _row("IOD Average", _temperature(runtime, "iod_average"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        _row("IOD Hotspot", _temperature(runtime, "iod_hotspot"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        _row("L3 Temp", _temperature(runtime, "l3"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        _row("VDDCR_VDD VRM", _temperature(runtime, "vdd_vrm"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        _row("VDDCR_SOC VRM", _temperature(runtime, "soc_vrm"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
        _row("VDD_MISC VRM", _temperature(runtime, "misc_vrm"),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True),
    ]


def _power_rows(runtime):
    """Build the Power Limits rows: the ZenStates limits, read-only."""
    rows = [
        _row(metric.label, _format_power(runtime, metric.key),
             THERMAL_POWER_CATEGORY, SENSOR_TAB, "Right", live=True)
        for metric in METRICS
        # Scalar is on Misc instead. It has no live half to pair with -- it
        # is a setting, and this window is for readings that move -- and two
        # rows of one name would collide, since names are how the tabs look
        # rows up across the profile.
        if metric.key != "scalar"
    ]
    # The thermal budget is not appended here: CPU Temp already reads Tctl
    # against its limit, in the same value-against-limit shape.
    return rows


# The System Info tab, in sections, the way the Intel tab reads. One column,
# the full width: the board row alone can want more than half the window, so
# a two-column split clips it.
#
# A row that is not named here would have no section at all, which is why the
# lookup below fails loudly rather than defaulting to a "General" heading --
# a row quietly filed under a leftover heading is one nobody notices.
SYSTEM_INFO_SECTIONS = (
    ("System", ("OS", "Platform")),
    ("Processor", ("CPU", "Code Name", "Vendor", "Technology",
                   "Cores / Threads", "Microcode")),
    ("Motherboard", ("Manufacturer", "Model", "BIOS", "BIOS Date",
                     "Chipset", "Southbridge", "LPCIO", "AGESA")),
    ("Clocks", ("BCLK", "MCLK", "FCLK", "UCLK", "DRAM Frequency",
                "UCLK:MCLK", "DRAM Ratio")),
    # What the memory is, then how much of it, then whose it is: the sockets
    # and channels sit together above the capacity they add up to, and the
    # part number leads the maker rows because it is what a kit is looked up
    # by.
    ("Memory", ("Type", "Slots Used", "Channels", "Memory Capacity",
                "DIMM Size", "Rank", "Part Number",
                "Module Manufacturer", "IC Manufacturer", "DRAM Die",
                "Serial Number", "Manufactured")),
    ("Graphics", ("GPU", "Board Manufacturer", "GPU Code Name",
                  "GPU Revision", "GPU Technology", "Cores", "ROPs / TMUs",
                  "Memory Size", "Memory Type", "Memory Vendor", "Bus Width",
                  "Resizable BAR", "Driver Version", "Driver Date")),
    ("Status", ("Status", "Read Status", "Training Status", "Voltage Status",
                "Power Status")),
)

SECTION_OF = {
    name: title for title, names in SYSTEM_INFO_SECTIONS for name in names
}


def build_timings(runtime):
    """Build the AM5-only UI table; all hardware values remain lazy."""
    def info(name, value, **extra):
        return _row(name, value, SECTION_OF[name], "System Info", **extra)

    # Sectioned the way the Intel tab is: what the machine is, the board, the
    # clock chain, what is installed in it, how the controller is set, the
    # card, and what could and could not be read. The rows are built in this
    # order because a category whose rows are not contiguous draws as two
    # headings with the same name.
    rows = [
        # System
        info("OS", lambda: _identity("os_name")),
        info("Platform", "AM5"),
        # Processor
        info("CPU", _system_info_value("cpu")),
        info("Code Name", _silicon_value(0)),
        # "Vendor", not "Manufacturer": the board takes that name below, the
        # way CPU-Z uses it, and two rows cannot share one.
        info("Vendor", _system_info_value("manufacturer")),
        info("Technology", _silicon_value(1)),
        info("Cores / Threads", _system_info_value("cores")),
        info("Microcode", lambda: _identity("microcode")),
        # Motherboard
        info("Manufacturer", _system_info_value("board_vendor")),
        info("Model", _system_info_value("board")),
        info("BIOS", _system_info_value("bios")),
        # Beside the firmware version it dates.
        info("BIOS Date", _system_info_value("bios_date")),
        info("Chipset", _chipset),
        info("Southbridge", _southbridge),
        info("LPCIO", lambda: _identity("lpcio_name")),
        info("AGESA", _format(runtime, "agesa")),
        # Clocks
        info("BCLK", _system_info_value("bclk")),
        info("MCLK", _format(runtime, "mclk_mhz", " MHz")),
        info("FCLK", _format(runtime, "fclk_mhz", " MHz")),
        info("UCLK", _format(runtime, "uclk_mhz", " MHz")),
        info("DRAM Frequency", lambda: _dram_frequency(runtime)),
        info("UCLK:MCLK", lambda: _uclk_ratio(runtime)),
        info("DRAM Ratio", lambda: _dram_ratio(runtime)),
        # Memory
        info("Type", lambda: _identity("memory_type")),
        # The sockets, then how they are wired, then the total they come to.
        info("Slots Used", _dimm_value("slots")),
        info("Channels", _dimm_value("channels")),
        info("Memory Capacity", _system_info_value("memory")),
        # What is installed, next to the total it adds up to.
        info("DIMM Size", _dimm_value("size")),
        info("Rank", _dimm_value("rank")),
        # The part number first, because that is what a kit is looked up by.
        # Then whose stick it is, then whose chips are on it: two different
        # companies, so two rows named for what each one made.
        info("Part Number", _dimm_value("part_number")),
        info("Module Manufacturer", _dimm_value("module_manufacturer")),
        info("IC Manufacturer", _dimm_value("dram_manufacturer")),
        info("DRAM Die", _dimm_value("dram_die")),
        info("Serial Number", _dimm_value("serial_number")),
        info("Manufactured", _dimm_value("manufacture_date")),
        # Graphics. The card's own name comes from the display class key
        # rather than WMI: it is the string the driver registered, which is
        # what GPU-Z and CPU-Z both show.
        info("GPU", _gpu_value("name")),
        info("Board Manufacturer", _gpu_value("board_manufacturer")),
        info("GPU Code Name", _gpu_value("code_name")),
        info("GPU Revision", _gpu_value("revision_text")),
        info("GPU Technology", _gpu_value("technology")),
        info("Cores", _gpu_value("cores")),
        info("ROPs / TMUs", _gpu_value("rops_tmus")),
        info("Memory Size", _gpu_value("memory_size")),
        info("Memory Type", _gpu_value("memory_type")),
        # Blank, and expected to stay blank: see radeon_gpu on why the memory
        # vendor is not reachable from anything this tool can read.
        info("Memory Vendor", _gpu_value("memory_vendor")),
        info("Bus Width", _gpu_value("bus_width")),
        # Read from the card's Resizable BAR capability, which reports the
        # size the BAR is programmed to. Nothing is written: the usual way to
        # size a BAR is to write all ones and read back the mask.
        info("Resizable BAR", _gpu_value("resizable_bar")),
        info("Driver Version", _gpu_value("driver_version")),
        info("Driver Date", _gpu_value("driver_date")),
        # Status. One line covering every transport. The four it replaces are
        # kept below, marked diagnostic: the tab shows the summary, the dump
        # keeps the full text including the APOB record addresses.
        info("Status", _status_summary(runtime), live=True),
        info("Read Status", lambda: _status(runtime), diagnostic=True),
        info("Training Status", lambda: _training_status(runtime),
             diagnostic=True),
        info("Voltage Status", lambda: _voltage_status(runtime),
             live=True, diagnostic=True),
        info("Power Status", lambda: _power_status(runtime),
             live=True, diagnostic=True),
    ]

    def misc(name, value, **extra):
        return _row(name, value, "Controller", MISC_TAB, **extra)

    # The controller settings, on their own page. They are neither identity
    # nor timing: what the controller was configured to do, which is a third
    # kind of thing and was the one section on System Info that did not
    # describe a part of the machine.
    rows.extend([
        misc("Refresh Mode", _format(runtime, "refresh_mode")),
        # The level behind the mode above. It sits here rather than among the
        # timings because it is a controller setting, and beside the row it
        # explains rather than in ZenTimings' position, which has no Misc tab
        # to put it on.
        misc("FGR", _format(runtime, "fgr")),
        misc("Gear Down Mode", _enabled(runtime, "gdm")),
        misc("Power Down Mode", _enabled(runtime, "powerdown")),
        misc("BGS", _enabled(runtime, "bgs")),
        misc("BGS Alt", _enabled(runtime, "bgs_alt")),
        misc("Nitro Rx/Tx/Ctrl", lambda: _nitro(runtime)),
    ])

    # What the processor was configured to allow, as opposed to what it is
    # drawing. The live halves stay on Telemetry, which keeps a maximum and
    # is where a limit is worth watching a load against; these are the
    # settings themselves and do not move.
    def limit(name, value):
        # Its own section rather than filed under Controller: a power ceiling
        # is not a setting the memory controller was given, and the two read
        # as different kinds of thing on the same page.
        return _row(name, value, LIMITS_CATEGORY, MISC_TAB, column="Left",
                    live=True)

    rows.extend([
        limit("Temp Limit", _thermal_limit_only(runtime)),
        limit("PPT Limit", _power_limit(runtime, "ppt")),
        limit("TDC Limit", _power_limit(runtime, "tdc")),
        limit("EDC Limit", _power_limit(runtime, "edc")),
        # Scalar keeps its own name: it is one value either way, so there is
        # no "limit" half to distinguish it from.
        limit("Scalar", _format_power(runtime, "scalar")),
    ])

    # Clocks lead the window, the way they do on the Intel side: what the
    # machine is running at, before what that costs it in heat and power.
    rows.extend(_clock_rows(runtime))
    # Temperatures lead the shared section, then the limits. What the CPU
    # reports about itself first, then what the board measures around it.
    rows.extend(_temperature_rows(runtime))
    rows.extend(_board_temperature_rows(runtime))
    rows.extend(_power_rows(runtime))
    rows.extend(_voltage_rows(runtime))
    rows.extend(_graphics_rows())
    rows.extend(_error_rows())

    primary = ("tCL", "tRCDRD", "tRCDWR", "tRP", "tRAS", "tRC")
    # Write recovery first, then the row-to-row and write-to-read pairs kept
    # long/short together, matching the Summary priority order.
    secondary = (
        "tWR", "tRRD_L", "tRRD_S", "tWTR_L", "tWTR_S", "tRTP", "tFAW", "tCWL"
    )
    # tRFCns precedes tRFC so Summary priority and Timings order match.
    refresh = ("tREFI", "tREFIns", "tRFCns", "tRFC", "tRFC2", "tRFCsb")

    # What used to be one 28-row "Tertiary" block, split into the groups a
    # tuner actually reads together and dealt across both columns so neither
    # one runs off the bottom of the tab. Order within a column is the order
    # these appear here.
    # Stagger sits last. The Timings tab orders its sections from
    # TIMINGS_SECTION_ORDER and is unaffected by this, but the Summary's
    # middle column follows the order rows appear here, and stagger belongs
    # after the CAS-to-CAS group there rather than splitting the turnarounds
    # from the same-direction groups.
    # Preamble stays ahead of the mode-register group so tRDPRE and tWRPRE
    # still precede tMOD, the order the Summary tail also uses.
    #
    # CAS to CAS stays ahead of Stagger for the Summary's sake, which is what
    # the note above is about: Power down sitting between them here costs
    # nothing, because tCKE and tXP are both in the Summary's priority list
    # and so never reach the leftovers that this order feeds.
    tertiary_left = (
        ("CAS to CAS", ("tCCD_L", "tCCD_L_WR", "tCCD_L_WR2")),
        ("Power down", ("tCKE", "tXP")),
        ("Stagger", ("tSTAG", "tSTAGsb")),
        ("Preamble / postamble", ("tRDPRE", "tRDPOST", "tWRPRE", "tWRPOST")),
        ("Mode register", ("tMRD", "tMOD", "tMRDPDA", "tMODPDA")),
    )
    tertiary_right = (
        ("Turnaround", ("tRDWR", "tWRRD")),
        ("Read to read", ("tRDRDSCL", "tRDRDSC", "tRDRDSD", "tRDRDDD")),
        ("Write to write", ("tWRWRSCL", "tWRWRSC", "tWRWRSD", "tWRWRDD")),
        ("PHY", ("tPHYWRD", "tPHYRDL", "tPHYWRL")),
    )

    def tertiary_rows(groups, column):
        for category, names in groups:
            for name in names:
                if name.startswith("tCCD_L"):
                    # All three come from the APOB run rather than the UMC;
                    # see amd_apob.find_ccdl_run for why the registers are
                    # only the fallback.
                    yield _ccdl_row(runtime, name, category)
                else:
                    yield _umc_row(runtime, name, category, column)

    # Timings tab layout. A section's column comes from its rows. The Timings
    # tab then sorts each column by TIMINGS_SECTION_ORDER, so the order here
    # is not what the tab draws -- it is what the Summary's middle column
    # follows, which is why CAS to CAS still precedes Stagger above.
    #   left   Primary -> Secondary -> tertiary_left
    #   right  Refresh timings -> tertiary_right
    rows.extend(
        _umc_row(runtime, name, "Primary", "Left") for name in primary
    )
    # The command rate closes the primary group. It reads as a timing -- 1T or
    # 2T is how long the controller holds a command -- so it belongs under tRC
    # rather than among the settings on the Misc tab, and the Summary already
    # places it in the same spot.
    rows.append(_row("CR", _format(runtime, "cmd_rate"), "Primary",
                     column="Left"))
    rows.extend(
        _umc_row(runtime, name, "Secondary", "Left") for name in secondary
    )
    for name in refresh:
        if name == "tREFIns":
            # The refresh interval in real time, beside the cycle count it is
            # derived from. Built by hand for the same reason tRFCns is: it
            # is two readings divided, not a register.
            row = _row(
                "tREFIns", _format_refi_ns(runtime), "Refresh timings",
                column="Right",
            )
            row.update({
                "value_a": _format_refi_ns(runtime, "cha"),
                "value_b": _format_refi_ns(runtime, "chb"),
                "name_a": "ChA",
                "name_b": "ChB",
            })
            rows.append(row)
        elif name == "tRFCns":
            # Derived from the active tRFC and MCLK, and it shows two intervals
            # outside Normal refresh mode, so it is built by hand.
            row = _row(
                "tRFCns", _format_rfc_ns(runtime), "Refresh timings",
                column="Right",
            )
            row.update({
                "value_a": _format_rfc_ns(runtime, "cha"),
                "value_b": _format_rfc_ns(runtime, "chb"),
                "name_a": "ChA",
                "name_b": "ChB",
            })
            rows.append(row)
        elif name == "tRFC":
            # Outside Normal mode the controller refreshes on tRFC2, so tRFC is
            # not in effect; dim it rather than implying it applies.
            rows.append(_umc_row(
                runtime, name, "Refresh timings", "Right",
                dim=lambda: not _refresh_is_normal(runtime),
            ))
        else:
            rows.append(
                _umc_row(runtime, name, "Refresh timings", "Right")
            )
    rows.extend(tertiary_rows(tertiary_left, "Left"))

    rtt_rows = (
        ("RTT WR", "rtt_wr"),
        ("RTT Nom WR", "rtt_nom_wr"),
        ("RTT Nom RD", "rtt_nom_rd"),
        ("RTT Park", "rtt_park"),
        ("RTT Park DQS", "rtt_park_dqs"),
    )
    odt_rows = (
        ("CA ODT A", "ca_odt_a"), ("CK ODT A", "ck_odt_a"),
        ("CS ODT A", "cs_odt_a"), ("CA ODT B", "ca_odt_b"),
        ("CK ODT B", "ck_odt_b"), ("CS ODT B", "cs_odt_b"),
    )
    drive_rows = (
        ("Proc ODT Pu", "proc_odt_pu"), ("Proc ODT Pd", "proc_odt_pd"),
        ("Proc CA DS", "proc_ca_ds"), ("Proc CK DS", "proc_ck_ds"),
        ("Proc CS DS", "proc_cs_ds"),
        # The one setting on the board's DDR Bus Configuration page that had
        # no row here. It heads the pull up and pull down pair because it is
        # what sets them: changing it in BIOS moved all three together.
        ("Proc DQ DS", "proc_dq_ds"),
        ("Proc DQ DS Pu", "proc_dq_ds_pu"),
        ("Proc DQ DS Pd", "proc_dq_ds_pd"),
        ("DRAM DQ DS Pu", "dram_dq_ds_pu"),
        ("DRAM DQ DS Pd", "dram_dq_ds_pd"),
    )
    # Skew, not Timings. These three groups are what memory training settled
    # on -- terminations, on-die termination and drive strengths -- rather
    # than intervals the controller was told to wait, and they filled the
    # Timings tab's right-hand column with twenty rows of a different kind of
    # thing. The Summary still gathers them by category, not by tab.
    rows.extend(
        _training_row(runtime, label, key, "RTT", "Left", tab=SKEW_TAB)
        for label, key in rtt_rows
    )
    rows.extend(
        _training_row(runtime, label, key, "ODT", "Left", tab=SKEW_TAB)
        for label, key in odt_rows
    )
    rows.extend(
        _training_row(runtime, label, key, "Drive Strength", "Right",
                      tab=SKEW_TAB)
        for label, key in drive_rows
    )
    rows.extend(tertiary_rows(tertiary_right, "Right"))
    return rows


def apply_formula(value, formula=None):
    if value is None:
        return EM_DASH
    return formula(value) if callable(formula) else value


RUNTIME = Am5Runtime()
TIMINGS = build_timings(RUNTIME)
