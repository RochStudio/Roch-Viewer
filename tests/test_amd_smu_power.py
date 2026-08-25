import os
import struct
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

from rochviewer.amd.power_metrics import (
    METRICS,
    METRICS_BY_KEY,
    format_power,
    validate_power,
)
from rochviewer.amd.smu_power import (
    CONFIRMED_POWER_OFFSETS,
    decode_power,
    decode_power_float,
)


def as_dword(value):
    return struct.unpack("<I", struct.pack("<f", float(value)))[0]


class PowerMetricTest(unittest.TestCase):
    def test_metrics_render_in_the_zenstates_order(self):
        self.assertEqual(
            [m.label for m in METRICS], ["PPT", "TDC", "EDC", "Scalar"]
        )
        self.assertEqual([m.unit for m in METRICS], ["W", "A", "A", "x"])

    def test_metric_keys_are_unique(self):
        keys = [m.key for m in METRICS]
        self.assertEqual(len(keys), len(set(keys)))
        self.assertEqual(len(METRICS_BY_KEY), len(METRICS))

    def test_validate_power_rejects_out_of_range(self):
        ppt = METRICS_BY_KEY["ppt"]
        self.assertAlmostEqual(validate_power(ppt, 162.0), 162.0)
        with self.assertRaises(ValueError):
            validate_power(ppt, 0.0)
        with self.assertRaises(ValueError):
            validate_power(ppt, 5000.0)
        with self.assertRaises(ValueError):
            validate_power(ppt, float("nan"))

    def test_format_shows_current_over_limit(self):
        ppt = METRICS_BY_KEY["ppt"]
        self.assertEqual(format_power(ppt, 41.0, 162.0), "41.0 / 162.0 W")
        self.assertEqual(format_power(ppt, None, 162.0), "162.0 W")
        self.assertEqual(format_power(ppt, 41.0, None), "41.0 W")
        self.assertIsNone(format_power(ppt, None, None))

    def test_scalar_uses_two_decimals(self):
        scalar = METRICS_BY_KEY["scalar"]
        self.assertEqual(format_power(scalar, None, 1.0), "1.00 x")


class PowerDecodeTest(unittest.TestCase):
    def test_confirmed_offsets_match_the_zenstates_reference(self):
        # PPT 162 W, TDC 120 A, EDC 180 A on MSI B850MPOWER + 9850X3D.
        self.assertEqual(
            CONFIRMED_POWER_OFFSETS,
            {
                "ppt": (0x00C, 0x008),
                "tdc": (0x024, 0x020),
                "edc": (0x100, 0x0FC),
            },
        )

    def test_scalar_is_never_claimed_as_a_table_offset(self):
        # 0x060 and 0x064 both hold exactly 1.0, which is too common a float to
        # claim on a value match alone. The scalar comes from the SMU getter
        # instead, so it must not reappear here as a guessed offset.
        self.assertNotIn("scalar", CONFIRMED_POWER_OFFSETS)

    def test_every_confirmed_key_names_a_real_metric(self):
        for key in CONFIRMED_POWER_OFFSETS:
            self.assertIn(key, METRICS_BY_KEY)

    def test_decode_power_float_rejects_non_finite(self):
        with self.assertRaises(ValueError):
            decode_power_float(as_dword(float("inf")))

    def test_decode_power_returns_captured_values_and_limits(self):
        table = {
            0x008: as_dword(162.0), 0x00C: as_dword(41.0244),
            0x020: as_dword(120.0), 0x024: as_dword(9.6954),
            0x0FC: as_dword(180.0), 0x100: as_dword(38.0),
        }
        values, limits = decode_power(lambda address: table[address], 0)
        self.assertEqual(limits, {"ppt": 162.0, "tdc": 120.0, "edc": 180.0})
        self.assertAlmostEqual(values["ppt"], 41.0244, places=3)
        self.assertAlmostEqual(values["tdc"], 9.6954, places=3)
        self.assertAlmostEqual(values["edc"], 38.0, places=3)

    def test_loaded_values_stay_inside_their_ranges(self):
        # Readings observed under an all-core load; if a range were too tight
        # the metric would blank out exactly when it matters.
        for key, value in (("ppt", 143.6983), ("tdc", 98.5523), ("edc", 100.0)):
            validate_power(METRICS_BY_KEY[key], value)

    def test_out_of_range_reading_is_dropped_not_shown(self):
        table = {0x008: as_dword(162.0), 0x00C: as_dword(9e9)}
        values, limits = decode_power(
            lambda address: table[address], 0, {"ppt": (0x00C, 0x008)}
        )
        self.assertEqual(limits, {"ppt": 162.0})
        self.assertNotIn("ppt", values)

    def test_a_failing_read_does_not_raise(self):
        def read(address):
            raise OSError("physical read refused")

        values, limits = decode_power(read, 0, {"ppt": (0x00C, 0x008)})
        self.assertEqual((values, limits), ({}, {}))

    def test_unknown_metric_keys_are_ignored(self):
        values, limits = decode_power(
            lambda address: as_dword(1.0), 0, {"not_a_metric": (0x10, 0x14)}
        )
        self.assertEqual((values, limits), ({}, {}))


class PboScalarTest(unittest.TestCase):
    """The scalar comes from RSMU 0x6D, not from a PM-table offset."""

    class FakeAccess:
        def __init__(self, arg0=as_dword(1.0), response=1):
            self.arg0 = arg0
            self.response = response
            self.commands = []
            self.arguments = []

        def read_response(self):
            return self.response

        def clear_response(self):
            pass

        def write_arguments(self, values):
            self.arguments.append(tuple(values))

        def issue_command(self, command):
            from rochviewer.amd.smu_clocks import PERMITTED_COMMANDS

            if command not in PERMITTED_COMMANDS:
                raise ValueError("command 0x%02X is not permitted" % command)
            self.commands.append(command)

        def read_arg0(self):
            return self.arg0

    def _reader(self, access):
        from rochviewer.amd.smu_voltages import RsmuVoltageReader

        reader = RsmuVoltageReader(access)
        reader._sleep = lambda _seconds: None
        return reader

    def test_the_getter_issues_the_scalar_command_with_no_arguments(self):
        access = self.FakeAccess()
        scalar = self._reader(access).pbo_scalar_in_run()
        self.assertEqual(access.commands, [0x6D])
        self.assertEqual(access.arguments, [(0, 0, 0, 0, 0, 0)])
        self.assertAlmostEqual(scalar, 1.0)

    def test_arg0_is_decoded_as_a_float(self):
        access = self.FakeAccess(arg0=as_dword(3.5))
        self.assertAlmostEqual(self._reader(access).pbo_scalar_in_run(), 3.5)

    def test_a_rejected_command_raises_rather_than_decoding_arg0(self):
        # A firmware that does not know the message answers non-OK; the row
        # must go blank instead of showing whatever arg0 held.
        access = self.FakeAccess(response=0xFF)
        with self.assertRaises(RuntimeError):
            self._reader(access).pbo_scalar_in_run()

    def test_the_scalar_command_is_permitted_and_nothing_else_is(self):
        from rochviewer.amd.smu_clocks import (
            PERMITTED_COMMANDS,
            RSMU_PBO_SCALAR_COMMAND,
        )

        self.assertEqual(RSMU_PBO_SCALAR_COMMAND, 0x6D)
        self.assertEqual(set(PERMITTED_COMMANDS), {0x05, 0x04, 0x03, 0x6D})
        # Neighbours of the getter are setters in the same message block.
        for forbidden in (0x6C, 0x6E, 0x00, 0xFF):
            self.assertNotIn(forbidden, PERMITTED_COMMANDS)

    def test_a_scalar_outside_its_range_is_dropped(self):
        from rochviewer.amd.smu_power import RsmuPowerReader

        reader = RsmuPowerReader(self.FakeAccess(arg0=as_dword(99.0)))
        reader._sleep = lambda _seconds: None
        self.assertIsNone(reader._read_scalar())

    def test_a_failing_getter_does_not_take_the_other_metrics_down(self):
        from rochviewer.amd.smu_power import RsmuPowerReader

        reader = RsmuPowerReader(self.FakeAccess(response=0xFF))
        reader._sleep = lambda _seconds: None
        self.assertIsNone(reader._read_scalar())


class PowerIsReadOnlyTest(unittest.TestCase):
    def test_module_exposes_no_write_path(self):
        from rochviewer.amd import smu_power as amd_smu_power

        source = open(amd_smu_power.__file__, encoding="utf-8").read()
        # ZenStates writes these limits; this project must only decode them.
        for forbidden in ("write_arguments", "issue_command", "SetPhysLong"):
            self.assertNotIn(forbidden, source)


if __name__ == "__main__":
    unittest.main()
