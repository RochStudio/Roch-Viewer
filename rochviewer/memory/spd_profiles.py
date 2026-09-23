# Roch Viewer -- a read-only memory-controller and timing viewer.
# Copyright (C) 2026 Roch Studio

"""Read and decode the information shown on the SPD tab.

The transport remains in the generation-specific SPD modules.  This module
turns their byte dictionaries into a small display model and joins each SPD
address to the physical slot name reported by SMBIOS.
"""

from __future__ import annotations

import math

from rochviewer.memory.ddr5_spd import EM_DASH


DDR4 = 0x0C
MTB_PS = 125
FTB_PS = 1

DDR4_MODULE_TYPES = {
    0x01: "RDIMM",
    0x02: "UDIMM",
    0x03: "SO-DIMM",
    0x04: "LRDIMM",
    0x05: "Mini-RDIMM",
    0x06: "Mini-UDIMM",
    0x08: "72b SO-RDIMM",
    0x09: "72b SO-UDIMM",
    0x0C: "16b SO-DIMM",
    0x0D: "32b SO-DIMM",
}


def _byte(values, offset, default=0):
    value = values.get(offset, default)
    return default if value is None else int(value) & 0xFF


def _signed_byte(value):
    value = int(value) & 0xFF
    return value - 0x100 if value & 0x80 else value


def _time_ps(values, coarse_offset, fine_offset=None, upper=0):
    coarse = (_byte(values, upper) << 8) | _byte(values, coarse_offset)
    fine = _signed_byte(_byte(values, fine_offset)) if fine_offset is not None else 0
    return coarse * MTB_PS + fine * FTB_PS


def _clock_cycles(time_ps, tck_ps):
    if not time_ps or not tck_ps:
        return None
    # XMP defines a 0.01-clock guardband before rounding up.
    return int(math.ceil((float(time_ps) / float(tck_ps)) - 0.01))


def _data_rate(tck_ps):
    raw = 2_000_000.0 / tck_ps
    jedec_bins = (800, 1066, 1333, 1600, 1866, 2133, 2400, 2666, 2933, 3200)
    nearest = min(jedec_bins, key=lambda value: abs(value - raw))
    return nearest if abs(nearest - raw) <= 40 else int(round(raw / 100.0) * 100)


def _supported_cls(values, start):
    result = []
    for byte_index in range(4):
        bits = _byte(values, start + byte_index)
        for bit in range(8):
            if bits & (1 << bit):
                result.append(7 + byte_index * 8 + bit)
    return result


def _profile(name, tck_ps, taa_ps, trcd_ps, trp_ps, tras_ps, trc_ps,
             voltage):
    if not tck_ps:
        return None
    clock_mhz = int(round(1_000_000.0 / tck_ps))
    data_rate = _data_rate(tck_ps)
    return {
        "name": name,
        "frequency": "%d MHz" % clock_mhz,
        "data_rate": "DDR4-%d" % data_rate,
        "cl": _clock_cycles(taa_ps, tck_ps),
        "trcd": _clock_cycles(trcd_ps, tck_ps),
        "trp": _clock_cycles(trp_ps, tck_ps),
        "tras": _clock_cycles(tras_ps, tck_ps),
        "trc": _clock_cycles(trc_ps, tck_ps),
        "voltage": "%.2f V" % voltage,
    }


def decode_ddr4_base(values):
    """Decode the module description and representative JEDEC profiles."""
    if _byte(values, 2) != DDR4:
        return {}

    density_code = _byte(values, 4) & 0x0F
    device_width_code = _byte(values, 12) & 0x07
    bus_width_code = _byte(values, 13) & 0x07
    ranks = ((_byte(values, 12) >> 3) & 0x07) + 1
    density_mbit = 256 << density_code if density_code <= 7 else 0
    device_width = 4 << device_width_code if device_width_code <= 3 else 0
    bus_width = 8 << bus_width_code if bus_width_code <= 3 else 0
    capacity_mb = (
        density_mbit * (bus_width // device_width) * ranks // 8
        if density_mbit and device_width and bus_width else 0
    )

    tck_ps = _time_ps(values, 18, 125)
    taa_ps = _time_ps(values, 24, 123)
    trcd_ps = _time_ps(values, 25, 122)
    trp_ps = _time_ps(values, 26, 121)
    upper = _byte(values, 27)
    tras_ps = (((upper & 0x0F) << 8) | _byte(values, 28)) * MTB_PS
    trc_ps = (((upper >> 4) << 8) | _byte(values, 29)) * MTB_PS
    trc_ps += _signed_byte(_byte(values, 120)) * FTB_PS

    profiles = []
    supported = _supported_cls(values, 20)
    # The highest three useful CAS choices are what compact SPD viewers show.
    # Lower CL values down-bin the same timing set; keeping the last three
    # avoids a wide table without hiding the module's fastest JEDEC choice.
    for cl in supported[-3:]:
        profile_tck = max(tck_ps, int(math.ceil(float(taa_ps) / cl)))
        item = _profile(
            "JEDEC %d" % cl, profile_tck, taa_ps, trcd_ps, trp_ps,
            tras_ps, trc_ps, 1.20,
        )
        if item:
            item["cl"] = cl
            profiles.append(item)

    module_type = DDR4_MODULE_TYPES.get(_byte(values, 3) & 0x0F)
    return {
        "memory_type": "DDR4",
        "module_type": module_type or "Type 0x%X" % (_byte(values, 3) & 0x0F),
        "capacity": "%d GB" % (capacity_mb // 1024) if capacity_mb else EM_DASH,
        "rank": "%dR" % ranks,
        "max_bandwidth": (
            "DDR4-%d (%d MHz)" % (
                _data_rate(tck_ps),
                int(round(1_000_000.0 / tck_ps)),
            ) if tck_ps else EM_DASH
        ),
        "profiles": profiles,
    }


def decode_ddr4_xmp(values):
    """Decode the two profile slots defined by Intel XMP 2.0."""
    if (_byte(values, 384), _byte(values, 385)) != (0x0C, 0x4A):
        return {"extension": EM_DASH, "profiles": []}
    revision = _byte(values, 387)
    result = {
        "extension": "XMP %d.%d" % (revision >> 4, revision & 0x0F),
        "profiles": [],
    }
    enabled = _byte(values, 386)
    for index, base in enumerate((393, 440)):
        if not (enabled & (1 << index)):
            continue
        fine = 431 if index == 0 else 478
        tck_ps = _time_ps(values, base + 3, fine)
        taa_ps = _time_ps(values, base + 8, fine - 1)
        trcd_ps = _time_ps(values, base + 9, fine - 2)
        trp_ps = _time_ps(values, base + 10, fine - 3)
        upper = _byte(values, base + 11)
        tras_ps = (((upper & 0x0F) << 8) | _byte(values, base + 12)) * MTB_PS
        trc_ps = (((upper >> 4) << 8) | _byte(values, base + 13)) * MTB_PS
        trc_ps += _signed_byte(_byte(values, fine - 4)) * FTB_PS
        voltage_byte = _byte(values, base)
        voltage = (1.0 if voltage_byte & 0x80 else 0.0) + (
            voltage_byte & 0x7F
        ) / 100.0
        profile = _profile(
            "XMP-%d" % _data_rate(tck_ps),
            tck_ps, taa_ps, trcd_ps, trp_ps, tras_ps, trc_ps, voltage,
        )
        if profile:
            result["profiles"].append(profile)
    return result


def decode_ddr4_spd(values, identity=None):
    """Return one complete DDR4 SPD display record."""
    record = decode_ddr4_base(values)
    if not record:
        return None
    record.update(identity or {})
    if not record.get("dram_die"):
        try:
            from rochviewer.memory.ddr5_spd import decode_die

            record["dram_die"] = decode_die(
                record.get("dram_manufacturer"), record.get("dram_stepping")
            )
        except Exception:
            record["dram_die"] = EM_DASH
    xmp = decode_ddr4_xmp(values)
    record["extension"] = xmp["extension"]
    record["profiles"].extend(xmp["profiles"])
    return record


# --- DDR5 (JESD400-5 base block, Intel XMP 3.0) ---------------------------
#
# DDR5 SPD states every timing in whole picoseconds, little-endian, rather
# than DDR4's medium/fine timebase pair. The offsets below were confirmed on
# an LGA1700 bench kit (V-Color TMXFL1680838KWK): the base block decodes to
# DDR5-4800 40-39-39-77, and XMP profile 1 to DDR5-8000 38-48-48-128 at
# 1.45 V, which is the kit's label and what the board trains it to.

DDR5 = 0x12

DDR5_MODULE_TYPES = {
    0x01: "RDIMM",
    0x02: "UDIMM",
    0x03: "SO-DIMM",
    0x04: "LRDIMM",
    0x05: "CUDIMM",
    0x06: "CSODIMM",
    0x07: "MRDIMM",
    0x08: "CAMM2",
}

# JEDEC DDR5 data rates, each with the tCK the standard rounds it to.
DDR5_SPEED_BINS = (
    (3200, 625), (3600, 555), (4000, 500), (4400, 454), (4800, 416),
    (5200, 384), (5600, 357), (6000, 333), (6400, 312), (6800, 294),
    (7200, 277), (7600, 263), (8000, 250), (8400, 238), (8800, 227),
)

DDR5_TCK_MIN = 20
DDR5_CAS_MASK = 24          # five bytes, bit n = CL 20 + 2n
DDR5_TAA = 30              # then tRCD, tRP, tRAS, tRC, two bytes each
DDR5_JEDEC_VOLTAGE = 1.10

XMP3_HEADER = 0x280
XMP3_PROFILES = (0x2C0, 0x300, 0x340, 0x380, 0x3C0)
# Within an XMP 3.0 profile: the voltages lead, then tCK, the CAS mask, and
# tAA onward in the same order as the base block.
XMP3_VDD = 1
XMP3_TCK = 5
XMP3_CAS_MASK = 7
XMP3_TAA = 0x0D
EXPO_HEADER = 0x340


def _word(values, offset):
    return _byte(values, offset) | (_byte(values, offset + 1) << 8)


def _ddr5_cycles(time_ps, tck_ps):
    """JESD400-5 rounding: nCK = trunc((t x 0.997 / tCK) + 1)."""
    if not time_ps or not tck_ps:
        return None
    return int((time_ps * 997 // tck_ps + 1000) // 1000)


def _ddr5_supported_cls(values, start):
    result = []
    for byte_index in range(5):
        bits = _byte(values, start + byte_index)
        for bit in range(8):
            if bits & (1 << bit):
                result.append(20 + 2 * (byte_index * 8 + bit))
    return result


def _ddr5_rate(tck_ps):
    """The standard data rate for a tCK, or the raw figure off the table."""
    for rate, bin_tck in DDR5_SPEED_BINS:
        if abs(bin_tck - tck_ps) <= 1:
            return rate
    return int(round(2_000_000.0 / tck_ps / 100.0) * 100)


def _ddr5_profile(name, values, taa, tck_ps, voltage, supported):
    """One table column: timings in clocks at ``tck_ps``.

    ``taa`` is where tAA sits; tRCD, tRP, tRAS and tRC follow it two bytes
    apart in the base block and in an XMP profile alike.
    """
    cl = _ddr5_cycles(_word(values, taa), tck_ps)
    if cl is None:
        return None
    # CAS latency only comes in the values the module lists as supported.
    if supported:
        cl = next((value for value in supported if value >= cl), cl)
    rate = _ddr5_rate(tck_ps)
    return {
        "name": name,
        # Half the data rate, as the speed is named: 416 ps is DDR5-4800 and
        # 2400 MHz, not the 2404 its rounded tCK works out to.
        "frequency": "%d MHz" % (rate // 2),
        "data_rate": "DDR5-%d" % rate,
        "cl": cl,
        "trcd": _ddr5_cycles(_word(values, taa + 2), tck_ps),
        "trp": _ddr5_cycles(_word(values, taa + 4), tck_ps),
        "tras": _ddr5_cycles(_word(values, taa + 6), tck_ps),
        "trc": _ddr5_cycles(_word(values, taa + 8), tck_ps),
        "voltage": "%.2f V" % voltage,
    }


def decode_ddr5_base(values):
    """Decode the module type and the fastest three JEDEC speed bins."""
    if _byte(values, 2) != DDR5:
        return {}
    tck_min = _word(values, DDR5_TCK_MIN)
    supported = _ddr5_supported_cls(values, DDR5_CAS_MASK)
    taa = _word(values, DDR5_TAA)

    profiles = []
    if tck_min:
        # From the module's own top bin down, each speed the standard defines
        # that it can run, as CPU-Z and Thaiphoon list them.
        for rate, bin_tck in sorted(DDR5_SPEED_BINS, reverse=True):
            if bin_tck < tck_min - 1:
                continue
            tck = max(bin_tck, tck_min)
            if supported and _ddr5_cycles(taa, tck) > supported[-1]:
                continue
            profile = _ddr5_profile(
                "JEDEC %d" % rate, values, DDR5_TAA, tck,
                DDR5_JEDEC_VOLTAGE, supported,
            )
            if profile:
                profiles.append(profile)
            if len(profiles) == 3:
                break

    module_code = _byte(values, 3) & 0x0F
    return {
        "memory_type": "DDR5",
        "module_type": DDR5_MODULE_TYPES.get(
            module_code, "Type 0x%X" % module_code
        ),
        "max_bandwidth": (
            "DDR5-%d (%d MHz)" % (
                _ddr5_rate(tck_min), _ddr5_rate(tck_min) // 2
            ) if tck_min else EM_DASH
        ),
        "profiles": profiles,
    }


def _xmp3_voltage(value):
    """Bits 6:5 are whole volts, bits 4:0 count 0.05 V: 0x29 is 1.45 V."""
    value = int(value) & 0xFF
    return ((value >> 5) & 0x03) + (value & 0x1F) * 0.05


def decode_ddr5_xmp(values):
    """Decode the enabled Intel XMP 3.0 profiles."""
    extensions = []
    profiles = []
    if (_byte(values, XMP3_HEADER), _byte(values, XMP3_HEADER + 1)) == (0x0C, 0x4A):
        revision = _byte(values, XMP3_HEADER + 2)
        extensions.append("XMP %d.%d" % (revision >> 4, revision & 0x0F))
        enabled = _byte(values, XMP3_HEADER + 3)
        for index, base in enumerate(XMP3_PROFILES):
            if not enabled & (1 << index):
                continue
            tck = _word(values, base + XMP3_TCK)
            if not tck:
                continue
            profile = _ddr5_profile(
                "XMP-%d" % _ddr5_rate(tck), values, base + XMP3_TAA, tck,
                _xmp3_voltage(_byte(values, base + XMP3_VDD)),
                _ddr5_supported_cls(values, base + XMP3_CAS_MASK),
            )
            if profile:
                profiles.append(profile)
    # An EXPO block shares the extension area on kits sold for both
    # platforms. Only its presence is reported: its profiles are AMD's to
    # describe, and this table is read on Intel.
    if bytes(_byte(values, EXPO_HEADER + i) for i in range(4)) == b"EXPO":
        extensions.append("EXPO")
    return {
        "extension": ", ".join(extensions) if extensions else EM_DASH,
        "profiles": profiles,
    }


def decode_ddr5_spd(values, identity=None):
    """Return one complete DDR5 SPD display record, or None."""
    record = decode_ddr5_base(values)
    if not record:
        return None
    xmp = decode_ddr5_xmp(values)
    record["extension"] = xmp["extension"]
    # Five columns in the table. XMP is the reason anybody opens this page,
    # so it keeps its columns and JEDEC gives way.
    jedec = record["profiles"][:max(0, 5 - len(xmp["profiles"]))]
    record["profiles"] = jedec + xmp["profiles"]
    record.update(identity or {})
    return record


def _join_slots(modules, inventory):
    """Attach SMBIOS physical slot labels without guessing from addresses."""
    remaining = sorted(
        list(inventory or []), key=lambda item: str(item.get("slot") or "")
    )
    for module in modules:
        match = None
        serial = str(module.get("serial_number") or "").strip()
        if serial not in ("", "0", EM_DASH):
            match = next((item for item in remaining
                          if str(item.get("serial_number") or "").strip() == serial), None)
        if match is None:
            part = str(module.get("part_number") or "").strip().upper()
            match = next((item for item in remaining
                          if str(item.get("part_number") or "").strip().upper() == part), None)
        if match is None and remaining:
            match = remaining[0]
        if match is not None:
            remaining.remove(match)
            module["slot"] = match.get("slot") or match.get("device_locator")
            for field in ("capacity", "rank"):
                if module.get(field) in (None, "", EM_DASH):
                    module[field] = match.get(field) or EM_DASH
            module_vendor = str(module.get("module_manufacturer") or "")
            if (not module_vendor or module_vendor == EM_DASH
                    or module_vendor.lower().startswith("0x")):
                module["module_manufacturer"] = (
                    match.get("module_manufacturer") or module_vendor or EM_DASH
                )
            # Do not fill the DRAM maker or die from the inventory's
            # part-number lookup. SPD is a readout page: if those bytes do
            # not identify the component, preserve the unavailable/raw value
            # instead of substituting a result inferred from a known kit.
        module.setdefault("slot", "SPD 0x%02X" % module.get("address", 0))
    return modules


def read_spd_modules(reader_factory=None, refresh=False):
    """Read installed SPD records once and return display-ready modules."""
    from rochviewer.memory.ddr_generation import detect_ddr_generation
    from rochviewer.memory.dimm_inventory import read_modules

    generation = detect_ddr_generation()
    modules = []
    if generation == "DDR4":
        from rochviewer.memory.ddr4_spd import read_modules as read_ddr4_modules

        modules = read_ddr4_modules(reader_factory=reader_factory, refresh=refresh)
    elif generation == "DDR5":
        from rochviewer.memory.ddr5_spd import read_identity, read_spd_bytes

        for identity in read_identity(reader_factory=reader_factory,
                                      refresh=refresh, generation="DDR5"):
            module = dict(identity)
            module.update({
                "memory_type": "DDR5",
                "module_type": EM_DASH,
                "capacity": EM_DASH,
                "rank": EM_DASH,
                "max_bandwidth": EM_DASH,
                "extension": EM_DASH,
                "profiles": [],
            })
            values = read_spd_bytes(
                identity.get("address"), identity.get("controller"),
                reader_factory=reader_factory,
            )
            # Identity is already decoded from the same read; only the
            # module description and the timing columns come from here.
            decoded = decode_ddr5_spd(values) if values else None
            if decoded:
                module.update(decoded)
            modules.append(module)
    try:
        inventory = read_modules(refresh=refresh)
    except Exception:
        inventory = []
    return _join_slots(modules, inventory)
