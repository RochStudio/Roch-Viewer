"""Run the Python AMD decoders over generated inputs, for the Rust parity test.

The profile parity fixtures compare whole tables; this compares the pure
decoders directly, over inputs no bench would produce: random UMC register
snapshots (missing registers, failed 0xFFFFFFFF reads, every refresh mode,
reserved preamble codes, out-of-range tCCD_L), random APOB training blocks in
both the Ryzen 9000 and the Ryzen 7000 layouts, and random tables for the
two tCCD_L searches. amd/decoder_parity.rs runs the Rust decoders over the
same inputs and requires the same answers.

The seed is fixed, so the fixture is reproducible. Run from the root of a
v1.1.0 checkout:

    python <this repository>/crates/rochviewer/src/amd/parity/dump_python_decoders.py
"""

import json
import os
import random
import struct
import sys

sys.path.insert(0, os.getcwd())

from rochviewer.amd import apob, timings  # noqa: E402

OUT = os.path.join(os.path.dirname(__file__), "decoder_parity.json")
rng = random.Random(20261016)
INVALID = 0xFFFFFFFF


def encode(value):
    """A decoded value in JSON, floats as their repr so both sides agree."""
    if isinstance(value, float):
        return {"float": repr(value)}
    return value


def encode_dict(values):
    if values is None:
        return None
    return {key: encode(value) for key, value in values.items()}


# -- UMC snapshots -----------------------------------------------------------

def register(offset):
    roll = rng.random()
    if roll < 0.08:
        return None                       # not read
    if roll < 0.13:
        return INVALID
    if offset == timings.REG_CONFIG:
        if rng.random() < 0.8:
            return (rng.randrange(1000, 6001) | (rng.randrange(4) << 17)
                    | (rng.randrange(1 << 13) << 19))
    if offset == timings.REG_RCD:
        if rng.random() < 0.85:
            return (rng.randrange(1, 64) | (rng.randrange(1, 128) << 8)
                    | (rng.randrange(1 << 8) << 16) | (rng.randrange(1 << 8) << 24))
    if offset in timings.RFC_OFFSETS:
        return rng.choice([0, 0, timings.RFC_PLACEHOLDER,
                           rng.randrange(1 << 32)])
    if offset in timings.RFCSB_OFFSETS:
        return rng.choice([0, 0x800, rng.randrange(1 << 11), rng.randrange(1 << 32)])
    if offset in (timings.REG_BGS0, timings.REG_BGS1):
        return rng.choice([0x87654321, 0x87654321, rng.randrange(1 << 32)])
    if offset in (timings.REG_PRE, timings.REG_PD, timings.REG_NITRO,
                  timings.REG_BGSA0, timings.REG_BGSA1):
        return rng.choice([rng.randrange(1 << 16), rng.randrange(1 << 32)])
    if offset in (timings.REG_CCDL, timings.REG_CCDL_WR2):
        return rng.choice([rng.randrange(256), rng.randrange(1 << 32)])
    return rng.randrange(1 << 32)


def snapshot():
    regs = {}
    for offset in timings.ALL_OFFSETS:
        value = register(offset)
        if value is not None:
            regs[offset] = value
    return regs


def umc_cases(count):
    cases = []
    for _ in range(count):
        regs = snapshot()
        cases.append({
            "regs": {"0x%X" % k: v for k, v in sorted(regs.items())},
            "decoded": encode_dict(timings.decode_channel(regs)),
            "misc": encode_dict(timings.decode_misc_settings(regs)),
        })
    return cases


# -- APOB training blocks ----------------------------------------------------

# The codes each table decodes, so most blocks pass plausibility and reach
# the lookups; the rest of each byte is random, and one block in ten keeps
# its RTT bytes all zero, which plausibility rejects.
RTT = list(range(8))
GROUP_ODT = list(range(8))
PROC_ODT = [0] + list(apob._PROC_ODT_OHMS)
DIRECT = list(apob._DIRECT_DS_OHMS)
DRAM = list(apob._DRAM_DQ_DS_OHMS)

GRANITE_FIELDS = {
    0x1A: RTT, 0x1B: RTT, 0x1C: RTT, 0x1D: RTT, 0x1E: RTT,
    0x08: GROUP_ODT, 0x09: GROUP_ODT, 0x0A: GROUP_ODT, 0x0B: GROUP_ODT,
    0x0C: GROUP_ODT, 0x0D: GROUP_ODT,
    0x0F: PROC_ODT, 0x11: DIRECT, 0x12: DIRECT, 0x13: DIRECT,
    0x1F: DRAM, 0x20: DRAM,
    0x21: PROC_ODT, 0x22: PROC_ODT, 0x23: PROC_ODT, 0x24: PROC_ODT,
}
RAPHAEL_FIELDS = {
    0x02: RTT, 0x03: RTT, 0x04: RTT, 0x05: RTT, 0x06: RTT, 0x07: DRAM,
    0x08: GROUP_ODT, 0x09: GROUP_ODT, 0x0A: GROUP_ODT, 0x0B: GROUP_ODT,
    0x0C: GROUP_ODT, 0x0D: GROUP_ODT,
    0x0E: PROC_ODT, 0x0F: PROC_ODT, 0x11: DIRECT, 0x12: DIRECT, 0x13: DIRECT,
}


def block(size, fields, rtt):
    data = bytearray(rng.randrange(256) for _ in range(size))
    data[0] = rng.choice([0, 1, 1, 1, rng.randrange(256)])
    data[1] = rng.choice([0, 1, 0, 1, rng.randrange(256)])
    valid = rng.choice([0.7, 0.9, 1.0])
    for offset, choices in fields.items():
        if offset < size and rng.random() < valid:
            data[offset] = rng.choice(choices)
    if rng.random() < 0.1:
        data[rtt] = bytes(5)
    return bytes(data)


def granite_cases(count):
    cases = []
    for _ in range(count):
        data = block(apob.GRANITE_RIDGE_BLOCK_SIZE, GRANITE_FIELDS, slice(0x1A, 0x1F))
        cases.append({"block": data.hex(),
                      "decoded": encode_dict(apob.decode_granite_ridge_training_block(data))})
    return cases


def raphael_cases(count):
    cases = []
    for _ in range(count):
        data = block(apob.RAPHAEL_BLOCK_SIZE, RAPHAEL_FIELDS, slice(0x02, 0x07))
        extended = (block(apob.RAPHAEL_BLOCK_SIZE, RAPHAEL_FIELDS, slice(0x02, 0x07))
                    if rng.random() < 0.5 else None)
        cases.append({
            "block": data.hex(),
            "extended": None if extended is None else extended.hex(),
            "decoded": encode_dict(apob.decode_raphael_training_block(data, extended)),
        })
    return cases


# -- the tCCD_L searches -----------------------------------------------------

def ccdl_table():
    data = bytearray(rng.randrange(256) for _ in range(rng.randrange(0, 160)))
    marker = struct.pack("<2H", *apob.CCDL_RUN_MARKER)
    run = (rng.randrange(6, 40), rng.randrange(6, 160), rng.randrange(6, 75))
    for _ in range(rng.randrange(0, 4)):
        if len(data) < 30:
            break
        at = rng.randrange(0, len(data) - 4) & ~(rng.randrange(2))
        data[at:at + 4] = marker
        start = at + apob.CCDL_RUN_OFFSET * 2
        if start + 6 <= len(data) and rng.random() < 0.8:
            struct.pack_into("<3H", data, start, *run)
    anchor = None
    if len(data) >= 6 and rng.random() < 0.6:
        at = rng.randrange(0, len(data) - 5) & ~1
        struct.pack_into("<3H", data, at, *run)
        anchor = run
    return bytes(data), anchor or run


def ccdl_cases(count):
    cases = []
    for _ in range(count):
        data, (tccdl, _wr, tccdl_wr2) = ccdl_table()
        if rng.random() < 0.1:
            tccdl = None
        if rng.random() < 0.1:
            tccdl_wr2 = 0
        run = apob.find_ccdl_run(data)
        cases.append({
            "table": data.hex(), "tccdl": tccdl, "tccdl_wr2": tccdl_wr2,
            "run": None if run is None else list(run),
            "wr": apob.find_ccdl_wr(data, tccdl, tccdl_wr2),
        })
    return cases


def main():
    result = {
        "umc": umc_cases(300),
        "granite_blocks": granite_cases(200),
        "raphael_blocks": raphael_cases(200),
        "ccdl": ccdl_cases(300),
    }
    with open(OUT, "w", encoding="utf-8", newline="\n") as handle:
        json.dump(result, handle, ensure_ascii=False, separators=(",", ":"))
        handle.write("\n")
    print("wrote %s: %s" % (OUT, ", ".join(
        "%s %d (%d decoded)" % (name, len(cases), sum(
            1 for case in cases if case.get("decoded", case.get("run")) is not None))
        for name, cases in result.items())))


if __name__ == "__main__":
    main()
