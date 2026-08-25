import hashlib
import inspect
import struct
import unittest
from unittest.mock import patch

from rochviewer.amd.apob import (
    ApobTableDiagnostic,
    GraniteRidgeApobReader,
    GraniteRidgeCandidate,
    decode_granite_ridge_training_block,
    enumerate_granite_ridge_candidates,
    extract_first_config_entry,
    find_ccdl_wr,
    parse_apob_channel_records,
    parse_apob_table,
)


def _training_block():
    block = bytearray(0x30)
    block[0x00] = 1
    block[0x08:0x0E] = bytes((0, 0, 1, 5, 5, 5))
    block[0x11:0x14] = bytes((40, 60, 30))
    block[0x1A:0x1F] = bytes((0, 0, 6, 5, 6))
    block[0x1F:0x21] = bytes((0, 1))
    block[0x21:0x25] = bytes((28, 30, 28, 30))
    return block


def _apob_table(block=None):
    block = bytes(block if block is not None else _training_block())
    table = bytearray(0x140)
    table[0:4] = b"APOB"
    struct.pack_into("<III", table, 4, 1, len(table), 0x60)
    first_offset = 0x80
    struct.pack_into("<I", table, 0x30, first_offset)
    struct.pack_into("<I", table, first_offset + 0x0C, 0x20)
    main_offset = first_offset + 0x20
    table[main_offset] = 0x01
    table[main_offset + 4] = 0x19
    struct.pack_into("<I", table, main_offset + 0x0C, 0x80)
    table[main_offset + 0x30:main_offset + 0x60] = block
    return bytes(table)


def _ambiguous_apob_table():
    """An APOB table whose memory-config block holds two plausible records."""
    table = bytearray(_apob_table())
    main_offset = 0xA0
    struct.pack_into("<I", table, main_offset + 0x0C, 0xA0)
    table[0x110:0x140] = _training_block()
    return bytes(table)


def _real_training_block():
    """Exact byte oracle from the 2026-08-06 B850MPOWER capture."""
    return bytes.fromhex(
        "01010000060506000000010505050C1E"
        "0C1E1E1E5D5834340100000006050602"
        "001E001E1E0000000000000000000000"
    )


def _duplicate_real_apob_table(second=None, gap=0):
    first = _real_training_block()
    second = first if second is None else bytes(second)
    table = bytearray(_apob_table(first))
    main_offset = 0xA0
    second_offset = 0xD0 + len(first) + gap
    main_end = second_offset + len(second)
    if main_end > len(table):
        table.extend(b"\0" * (main_end - len(table)))
    struct.pack_into("<I", table, 0x08, len(table))
    struct.pack_into("<I", table, main_offset + 0x0C, main_end - main_offset)
    table[second_offset:second_offset + len(second)] = second
    return bytes(table)


def _distinct_channel_apob_table():
    """Exact observed geometry with deliberately distinct valid channel data."""
    channel_b = bytearray(_real_training_block())
    channel_b[0x0A] = 2   # Group-A CA ODT: 240 ohms
    channel_b[0x0D] = 6   # Group-B CA ODT: 48 ohms
    channel_b[0x1C] = 4   # RTT WR: RZQ/4 (60 ohms)
    channel_b[0x21] = 12  # Processor ODT pull-up: 60 ohms
    return _duplicate_real_apob_table(channel_b)


class ProcDqDriveStrengthTest(unittest.TestCase):
    """0x0F was confirmed by changing the BIOS setting, not by resemblance.

    Processor DQ drive strengths went from Auto to 40 ohm across a reboot and
    this byte followed, 30 -> 28. Three bytes in the whole block moved: this
    one and the P0 pull up and pull down pair, which is the same setting
    reaching the same pins. Nothing else stirred, so it is not a counter.
    """

    def test_the_auto_reading_decodes(self):
        block = bytearray(0x30)
        block[0x0F] = 30
        self.assertEqual(_proc_odt_of(block), "34.3 Ω")

    def test_the_setting_that_was_dialled_in_decodes(self):
        block = bytearray(0x30)
        block[0x0F] = 28
        self.assertEqual(_proc_odt_of(block), "40 Ω")

    def test_high_impedance_is_an_option_on_this_board(self):
        # The board's dropdown offers it beside the impedances, so zero here
        # is a choice rather than an unread byte.
        block = bytearray(0x30)
        block[0x0F] = 0
        self.assertEqual(_proc_odt_of(block), "Hi-Z")


def _proc_odt_of(block):
    """Decode just the DQ drive strength from an otherwise valid block."""
    block[0x08:0x0E] = bytes((0, 0, 1, 5, 5, 5))
    block[0x11:0x14] = bytes((40, 60, 30))
    block[0x1A:0x1F] = bytes((0, 0, 6, 5, 6))
    block[0x1F:0x21] = bytes((0, 1))
    block[0x21:0x25] = bytes((28, 30, 28, 30))
    return decode_granite_ridge_training_block(block)["proc_dq_ds"]


class GraniteRidgeTrainingDecodeTest(unittest.TestCase):
    def test_decodes_rtt_group_odt_and_drive_strengths(self):
        block = bytearray(0x30)
        block[0x01] = 0                  # GDM
        block[0x08:0x0E] = bytes((0, 0, 1, 5, 5, 5))
        block[0x0F] = 28                 # Processor DQ drive strengths
        block[0x11:0x14] = bytes((40, 60, 30))
        block[0x1A:0x1F] = bytes((0, 0, 6, 5, 6))
        block[0x1F:0x21] = bytes((0, 1))
        block[0x21:0x25] = bytes((28, 30, 28, 30))

        decoded = decode_granite_ridge_training_block(block)

        self.assertEqual(decoded["rtt_nom_wr"], "Off")
        self.assertEqual(decoded["rtt_nom_rd"], "Off")
        self.assertEqual(decoded["rtt_wr"], "RZQ/6 (40 Ω)")
        self.assertEqual(decoded["rtt_park"], "RZQ/5 (48 Ω)")
        self.assertEqual(decoded["rtt_park_dqs"], "RZQ/6 (40 Ω)")
        self.assertEqual(decoded["ca_odt_a"], "480 Ω")
        self.assertEqual(decoded["ck_odt_a"], "Off")
        self.assertEqual(decoded["cs_odt_a"], "Off")
        self.assertEqual(decoded["ca_odt_b"], "60 Ω")
        self.assertEqual(decoded["ck_odt_b"], "60 Ω")
        self.assertEqual(decoded["cs_odt_b"], "60 Ω")
        self.assertEqual(decoded["proc_odt_pu"], "40 Ω")
        self.assertEqual(decoded["proc_odt_pd"], "34.3 Ω")
        self.assertEqual(decoded["proc_ca_ds"], "40 Ω")
        self.assertEqual(decoded["proc_ck_ds"], "60 Ω")
        self.assertEqual(decoded["proc_cs_ds"], "30 Ω")
        self.assertEqual(decoded["proc_dq_ds"], "40 Ω")
        self.assertEqual(decoded["proc_dq_ds_pu"], "40 Ω")
        self.assertEqual(decoded["proc_dq_ds_pd"], "34.3 Ω")
        self.assertEqual(decoded["dram_dq_ds_pu"], "34 Ω")
        self.assertEqual(decoded["dram_dq_ds_pd"], "40 Ω")


class ApobTableParseTest(unittest.TestCase):
    def test_finds_valid_training_record_inside_apob_container(self):
        parsed = parse_apob_table(_apob_table())
        self.assertEqual(parsed.record_offset, 0xD0)
        self.assertEqual(parsed.values["rtt_wr"], "RZQ/6 (40 Ω)")
        self.assertEqual(parsed.values["ca_odt_b"], "60 Ω")

    def test_rejects_ambiguous_multiple_training_records(self):
        table = bytearray(_apob_table())
        main_offset = 0xA0
        struct.pack_into("<I", table, main_offset + 0x0C, 0xA0)
        table[0x110:0x140] = _training_block()
        with self.assertRaisesRegex(ValueError, "multiple"):
            parse_apob_table(table)

    def test_accepts_contiguous_byte_identical_duplicate_records(self):
        table = _duplicate_real_apob_table()

        parsed = parse_apob_table(table)

        self.assertEqual(parsed.record_offset, 0xD0)
        self.assertEqual(parsed.values["rtt_wr"], "RZQ/6 (40 Ω)")

    def test_rejects_same_decoding_when_an_ignored_raw_byte_differs(self):
        second = bytearray(_real_training_block())
        second[0x25] = 1
        table = _duplicate_real_apob_table(second)
        candidates = enumerate_granite_ridge_candidates(table).candidates
        self.assertEqual(candidates[0].values, candidates[1].values)

        with self.assertRaisesRegex(ValueError, "multiple"):
            parse_apob_table(table)

    def test_rejects_identical_records_when_not_contiguous(self):
        table = _duplicate_real_apob_table(gap=0x10)

        with self.assertRaisesRegex(ValueError, "multiple"):
            parse_apob_table(table)


class ApobChannelParseTest(unittest.TestCase):
    def test_maps_exact_two_record_geometry_to_distinct_channels(self):
        parsed = parse_apob_channel_records(_distinct_channel_apob_table())

        self.assertEqual(parsed.channel_a.record_offset, 0xD0)
        self.assertEqual(parsed.channel_b.record_offset, 0x100)
        self.assertEqual(parsed.channel_a.values["rtt_wr"], "RZQ/6 (40 Ω)")
        self.assertEqual(parsed.channel_b.values["rtt_wr"], "RZQ/4 (60 Ω)")
        self.assertEqual(parsed.channel_a.values["ca_odt_a"], "480 Ω")
        self.assertEqual(parsed.channel_b.values["ca_odt_a"], "240 Ω")
        self.assertEqual(parsed.channel_a.values["ca_odt_b"], "60 Ω")
        self.assertEqual(parsed.channel_b.values["ca_odt_b"], "48 Ω")

    def test_maps_byte_identical_boundary_records_as_two_channels(self):
        parsed = parse_apob_channel_records(_duplicate_real_apob_table())

        self.assertEqual(parsed.channel_a.record_offset, 0xD0)
        self.assertEqual(parsed.channel_b.record_offset, 0x100)
        self.assertEqual(parsed.channel_a.values, parsed.channel_b.values)

    def test_rejects_wrong_main_geometry_even_with_two_valid_records(self):
        with self.assertRaisesRegex(ValueError, "channel geometry"):
            parse_apob_channel_records(_duplicate_real_apob_table(gap=0x10))

    def test_rejects_exact_size_when_only_one_boundary_record_is_valid(self):
        table = _duplicate_real_apob_table(second=b"\0" * 0x30)

        with self.assertRaisesRegex(ValueError, "channel geometry"):
            parse_apob_channel_records(table)

    def test_rejects_extra_or_misaligned_candidate(self):
        table = _duplicate_real_apob_table()
        diagnostic = enumerate_granite_ridge_candidates(table)
        extra = GraniteRidgeCandidate(
            diagnostic.scan_start + 1,
            diagnostic.candidates[0].raw,
            diagnostic.candidates[0].values,
        )
        polluted = ApobTableDiagnostic(
            table_size=diagnostic.table_size,
            header_size=diagnostic.header_size,
            sha256=diagnostic.sha256,
            config_offsets=diagnostic.config_offsets,
            first_offset=diagnostic.first_offset,
            first_size=diagnostic.first_size,
            main_offset=diagnostic.main_offset,
            main_size=diagnostic.main_size,
            scan_start=diagnostic.scan_start,
            scan_end=diagnostic.scan_end,
            candidates=diagnostic.candidates + (extra,),
        )
        with patch("rochviewer.amd.apob.enumerate_granite_ridge_candidates", return_value=polluted):
            with self.assertRaisesRegex(ValueError, "channel geometry"):
                parse_apob_channel_records(table)

    def test_channel_parser_accepts_only_table_bytes(self):
        self.assertEqual(
            list(inspect.signature(parse_apob_channel_records).parameters),
            ["table"],
        )


class EnumerateGraniteRidgeCandidatesTest(unittest.TestCase):
    def test_extracts_only_bounded_first_general_config_entry(self):
        table = _apob_table()

        offset, raw = extract_first_config_entry(table)

        self.assertEqual(offset, 0x80)
        self.assertEqual(raw, table[0x80:0xA0])

    def test_first_config_extraction_rejects_oversized_entry(self):
        table = bytearray(_apob_table())
        struct.pack_into("<I", table, 0x80 + 0x0C, 0x1001)
        with self.assertRaisesRegex(ValueError, "limit"):
            extract_first_config_entry(table)

    def test_single_record_table_reports_one_candidate_with_metadata(self):
        table = _apob_table()

        diag = enumerate_granite_ridge_candidates(table)

        self.assertIsInstance(diag, ApobTableDiagnostic)
        self.assertEqual(diag.table_size, len(table))
        self.assertEqual(diag.header_size, 0x60)
        self.assertEqual(diag.config_offsets, (0x80,))
        self.assertEqual(diag.first_offset, 0x80)
        self.assertEqual(diag.first_size, 0x20)
        self.assertEqual(diag.main_offset, 0xA0)
        self.assertEqual(diag.main_size, 0x80)
        self.assertEqual(diag.scan_start, 0xD0)
        self.assertEqual(diag.sha256, hashlib.sha256(table).hexdigest())
        self.assertEqual(len(diag.candidates), 1)
        candidate = diag.candidates[0]
        self.assertIsInstance(candidate, GraniteRidgeCandidate)
        self.assertEqual(candidate.record_offset, 0xD0)
        self.assertEqual(len(candidate.raw), 0x30)
        self.assertEqual(candidate.raw, table[0xD0:0x100])
        self.assertEqual(candidate.values["rtt_wr"], "RZQ/6 (40 Ω)")

    def test_enumerates_every_plausible_candidate_without_selecting(self):
        table = _ambiguous_apob_table()

        diag = enumerate_granite_ridge_candidates(table)

        self.assertEqual(
            [candidate.record_offset for candidate in diag.candidates],
            [0xD0, 0xF4, 0x110],
        )
        self.assertEqual(
            [candidate.values["rtt_park"] for candidate in diag.candidates],
            ["RZQ/5 (48 Ω)", "Off", "RZQ/5 (48 Ω)"],
        )
        for candidate in diag.candidates:
            self.assertEqual(len(candidate.raw), 0x30)
        self.assertEqual(diag.main_offset, 0xA0)
        self.assertEqual(diag.main_size, 0xA0)
        self.assertEqual(diag.sha256, hashlib.sha256(table).hexdigest())
        # Enumeration is evidence only: the parser must still refuse to pick one.
        with self.assertRaisesRegex(ValueError, "multiple"):
            parse_apob_table(table)

    def test_enumerate_accepts_only_a_table_no_address_or_write_input(self):
        params = list(
            inspect.signature(enumerate_granite_ridge_candidates).parameters
        )
        self.assertEqual(params, ["table"])

    def test_enumerate_rejects_invalid_header(self):
        with self.assertRaises(ValueError):
            enumerate_granite_ridge_candidates(b"\x00" * 0x80)


class ApobPhysicalReaderTest(unittest.TestCase):
    def test_skips_invalid_candidate_and_reads_bounded_valid_table(self):
        table = _apob_table()
        valid_base = 0x2000
        dwords = {
            valid_base + offset: int.from_bytes(
                table[offset:offset + 4].ljust(4, b"\0"), "little"
            )
            for offset in range(0, len(table), 4)
        }
        calls = []

        def read_dword(address):
            calls.append(address)
            return dwords.get(address, 0)

        reader = GraniteRidgeApobReader(
            read_dword=read_dword,
            candidate_addresses=(0x1000, valid_base),
        )
        values = reader.read()

        self.assertEqual(values["rtt_park"], "RZQ/5 (48 Ω)")
        self.assertEqual(reader.table_address, valid_base)
        self.assertEqual(reader.record_address, valid_base + 0xD0)
        self.assertIn(0x1000, calls)
        self.assertLessEqual(max(calls), valid_base + len(table) - 4)

    def test_oversized_table_is_rejected_before_body_read(self):
        base = 0x3000
        header = bytearray(0x10)
        header[:4] = b"APOB"
        struct.pack_into("<II", header, 0x08, 0x400001, 0x60)
        dwords = {
            base + offset: int.from_bytes(header[offset:offset + 4], "little")
            for offset in range(0, len(header), 4)
        }
        calls = []

        def read_dword(address):
            calls.append(address)
            return dwords.get(address, 0)

        reader = GraniteRidgeApobReader(
            read_dword=read_dword,
            candidate_addresses=(base,),
        )
        self.assertIsNone(reader.read())
        self.assertEqual(max(calls), base + 0x0C)
        self.assertIn("outside the APOB limit", reader.last_error)

    def test_rejects_header_that_changes_during_body_read(self):
        table = _apob_table()
        base = 0x4000
        dwords = {
            base + offset: int.from_bytes(
                table[offset:offset + 4].ljust(4, b"\0"), "little"
            )
            for offset in range(0, len(table), 4)
        }
        base_reads = 0

        def read_dword(address):
            nonlocal base_reads
            if address == base:
                base_reads += 1
                if base_reads >= 3:
                    return int.from_bytes(b"BAD!", "little")
            return dwords.get(address, 0)

        reader = GraniteRidgeApobReader(
            read_dword=read_dword,
            candidate_addresses=(base,),
        )
        self.assertIsNone(reader.read())
        self.assertIn("changed", reader.last_error)

    def test_rejects_multiple_valid_apob_candidate_bases(self):
        table = _apob_table()
        bases = (0x5000, 0x7000)
        dwords = {}
        for base in bases:
            dwords.update({
                base + offset: int.from_bytes(
                    table[offset:offset + 4].ljust(4, b"\0"), "little"
                )
                for offset in range(0, len(table), 4)
            })
        reader = GraniteRidgeApobReader(
            read_dword=lambda address: dwords.get(address, 0),
            candidate_addresses=bases,
        )
        self.assertIsNone(reader.read())
        self.assertIn("multiple", reader.last_error)

    def test_preserves_ambiguous_candidate_diagnostics(self):
        table = _ambiguous_apob_table()
        base = 0x6000
        dwords = {
            base + offset: int.from_bytes(
                table[offset:offset + 4].ljust(4, b"\0"), "little"
            )
            for offset in range(0, len(table), 4)
        }
        reader = GraniteRidgeApobReader(
            read_dword=lambda address: dwords.get(address, 0),
            candidate_addresses=(base,),
        )

        self.assertIsNone(reader.read())
        self.assertIn("multiple", reader.last_error)
        self.assertEqual(len(reader.ambiguous_candidates), 1)
        address, diag = reader.ambiguous_candidates[0]
        self.assertEqual(address, base)
        self.assertIsInstance(diag, ApobTableDiagnostic)
        self.assertEqual(
            [candidate.record_offset for candidate in diag.candidates],
            [0xD0, 0xF4, 0x110],
        )
        self.assertEqual(diag.sha256, hashlib.sha256(table).hexdigest())

    def test_consensus_duplicates_do_not_leave_ambiguity_diagnostics(self):
        table = _duplicate_real_apob_table()
        base = 0x6800
        dwords = {
            base + offset: int.from_bytes(
                table[offset:offset + 4].ljust(4, b"\0"), "little"
            )
            for offset in range(0, len(table), 4)
        }
        reader = GraniteRidgeApobReader(
            read_dword=lambda address: dwords.get(address, 0),
            candidate_addresses=(base,),
        )

        values = reader.read()

        self.assertEqual(values["proc_odt_pu"], "34.3 Ω")
        self.assertEqual(reader.record_address, base + 0xD0)
        self.assertEqual(reader.ambiguous_candidates, ())

    def test_reader_exposes_exact_geometry_as_separate_channels(self):
        table = _distinct_channel_apob_table()
        base = 0x6C00
        dwords = {
            base + offset: int.from_bytes(
                table[offset:offset + 4].ljust(4, b"\0"), "little"
            )
            for offset in range(0, len(table), 4)
        }
        reader = GraniteRidgeApobReader(
            read_dword=lambda address: dwords.get(address, 0),
            candidate_addresses=(base,),
        )

        values = reader.read()

        self.assertEqual(values["rtt_wr"], "RZQ/6 (40 Ω)")
        self.assertEqual(reader.channel_values["cha"]["rtt_wr"], "RZQ/6 (40 Ω)")
        self.assertEqual(reader.channel_values["chb"]["rtt_wr"], "RZQ/4 (60 Ω)")
        self.assertEqual(reader.channel_values["cha"]["ca_odt_a"], "480 Ω")
        self.assertEqual(reader.channel_values["chb"]["ca_odt_a"], "240 Ω")
        self.assertEqual(reader.channel_record_addresses["cha"], base + 0xD0)
        self.assertEqual(reader.channel_record_addresses["chb"], base + 0x100)
        self.assertEqual(reader.ambiguous_candidates, ())

    def test_reader_starts_with_no_ambiguous_candidates(self):
        reader = GraniteRidgeApobReader(read_dword=lambda _address: 0)
        self.assertEqual(reader.ambiguous_candidates, ())

    def test_reader_exposes_no_ccdl_write_path(self):
        # find_ccdl_wr only ever reads the table it is handed.
        source = inspect.getsource(find_ccdl_wr)
        self.assertNotIn("write", source.lower())

    def test_reader_exposes_no_write_operation(self):
        reader = GraniteRidgeApobReader(read_dword=lambda _address: 0)
        public_names = {name.lower() for name in dir(reader)}
        self.assertNotIn("write", public_names)
        self.assertNotIn("write_dword", public_names)
        self.assertNotIn("setphyslong", public_names)


class FindCcdlWrTest(unittest.TestCase):
    """tCCD_L_WR, anchored on the two tCCD_L values the UMC does report."""

    @staticmethod
    def _table(*runs, pad=0x40):
        # 16-bit little-endian values, the way the APOB stores the run, with
        # unrelated data on both sides.
        data = bytearray(struct.pack("<%dH" % pad, *range(200, 200 + pad)))
        for run in runs:
            data.extend(struct.pack("<%dH" % len(run), *run))
            data.extend(b"\x00\x11" * 8)
        return bytes(data)

    def test_the_value_between_the_two_anchors_is_returned(self):
        # The bench run: 58, 8, 21, 58, 48, 60, 8, 21, 83, 42, 130, 58
        table = self._table((58, 8, 21, 58, 48, 60, 8, 21, 83, 42, 130, 58))
        self.assertEqual(find_ccdl_wr(table, 21, 42), 83)

    def test_both_channel_runs_agreeing_is_still_one_answer(self):
        run = (8, 21, 83, 42, 130)
        self.assertEqual(find_ccdl_wr(self._table(run, run), 21, 42), 83)

    def test_disagreeing_matches_report_nothing(self):
        # A coincidental 21/42 pair elsewhere must not be resolved by picking
        # one; without a single answer there is no answer.
        table = self._table((8, 21, 83, 42, 0), (8, 21, 24, 42, 0))
        self.assertIsNone(find_ccdl_wr(table, 21, 42))

    def test_a_value_outside_the_ratio_bound_is_rejected(self):
        self.assertIsNone(find_ccdl_wr(self._table((21, 400, 42)), 21, 42))
        self.assertIsNone(find_ccdl_wr(self._table((21, 3, 42)), 21, 42))

    def test_no_match_reports_nothing(self):
        self.assertIsNone(find_ccdl_wr(self._table((8, 9, 10)), 21, 42))

    def test_missing_inputs_report_nothing(self):
        table = self._table((21, 83, 42))
        self.assertIsNone(find_ccdl_wr(b"", 21, 42))
        self.assertIsNone(find_ccdl_wr(table, None, 42))
        self.assertIsNone(find_ccdl_wr(table, 21, None))

    def test_a_short_table_does_not_raise(self):
        self.assertIsNone(find_ccdl_wr(b"\x15\x00\x53", 21, 42))


if __name__ == "__main__":
    unittest.main()
