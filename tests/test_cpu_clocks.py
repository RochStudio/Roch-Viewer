"""Cover the core-clock reader: what it derives, and what it refuses."""

import unittest

import cpu_clocks
from cpu_clocks import (
    MAX_NOMINAL_MHZ,
    MAX_PERFORMANCE_PERCENT,
    MIN_SAMPLE_INTERVAL_S,
    _instance_order,
    _per_core,
    _total,
    core_labels,
    processor_label,
    read_clocks,
    read_core_clocks,
)

# One collection from the Z790-P bench, idling: sixteen logical processors
# around 176% of a 3187 MHz nominal, plus the two aggregate instances PDH
# adds. Utility is the same scale with idle time counted.
BENCH = {
    "performance": (
        [("0,%d" % core, 176.0 + core * 0.1) for core in range(16)]
        + [("0,_Total", 173.8), ("_Total", 173.8)]
    ),
    "utility": (
        [("0,%d" % core, 2.0) for core in range(16)]
        + [("0,_Total", 2.9), ("_Total", 2.9)]
    ),
    "frequency": [("_Total", 3187.0)],
}


def bench(overrides=None):
    rows = dict(BENCH)
    rows.update(overrides or {})
    return lambda: rows


class InstanceSelectionTest(unittest.TestCase):
    def test_the_total_is_the_machine_wide_instance(self):
        self.assertEqual(_total(BENCH["performance"]), 173.8)

    def test_the_group_total_is_not_mistaken_for_the_machine(self):
        # PDH names them "_Total" and "0,_Total". Matching on a suffix would
        # take whichever came first.
        self.assertEqual(_total([("0,_Total", 1.0), ("_Total", 2.0)]), 2.0)

    def test_per_core_excludes_every_aggregate(self):
        cores = _per_core(BENCH["performance"])
        self.assertEqual(len(cores), 16)
        self.assertNotIn(173.8, cores)

    def test_a_wrapped_counter_is_not_a_clock(self):
        rows = [("0,0", MAX_PERFORMANCE_PERCENT + 1), ("0,1", 176.0)]
        self.assertEqual(_per_core(rows), [176.0])

    def test_the_nominal_frequency_has_its_own_bound(self):
        # It comes through the same shape in megahertz, not as a percentage.
        # Sharing the percentage bound rejected a good 3187 MHz and left every
        # clock row blank.
        self.assertGreater(BENCH["frequency"][0][1], MAX_PERFORMANCE_PERCENT)
        self.assertEqual(
            _total(BENCH["frequency"], MAX_NOMINAL_MHZ), 3187.0
        )
        self.assertIsNone(_total(BENCH["frequency"]))


class ReadClocksTest(unittest.TestCase):
    def test_the_bench_sample_gives_the_clocks_it_was_running(self):
        found = read_clocks(collect=bench())
        self.assertAlmostEqual(found["core_avg"], 3187.0 * 1.738, places=1)
        self.assertAlmostEqual(found["core_max"], 3187.0 * 1.775, places=1)
        self.assertAlmostEqual(found["core_effective"], 3187.0 * 0.029,
                               places=1)

    def test_the_maximum_is_a_core_and_not_the_average(self):
        found = read_clocks(collect=bench())
        self.assertGreater(found["core_max"], found["core_avg"])

    def test_the_effective_clock_is_far_below_the_core_clock_at_idle(self):
        # The distinction the two rows exist to show: the cores run at 5.5 GHz
        # when they run, and hardly run at all.
        found = read_clocks(collect=bench())
        self.assertLess(found["core_effective"], found["core_avg"] / 10)

    def test_no_sample_reports_nothing_rather_than_zero(self):
        self.assertEqual(read_clocks(collect=lambda: {}), {})

    def test_a_sample_without_a_nominal_frequency_reports_nothing(self):
        # Every value here is a percentage of it, so it is not optional.
        self.assertEqual(read_clocks(collect=bench({"frequency": []})), {})

    def test_a_missing_utility_counter_does_not_lose_the_core_clocks(self):
        found = read_clocks(collect=bench({"utility": []}))
        self.assertIn("core_avg", found)
        self.assertNotIn("core_effective", found)


class PerCoreTest(unittest.TestCase):
    """The breakdown behind the aggregate row."""

    def test_one_entry_per_logical_processor(self):
        cores = read_core_clocks(collect=bench())
        self.assertEqual(len(cores), 16)
        self.assertEqual([label for label, _mhz in cores],
                         ["CPU %d" % i for i in range(16)])

    def test_the_values_are_the_utility_counter_not_the_performance_one(self):
        # Effective clock counts idle time; the performance counter does not.
        # Reading the wrong one would put 5.5 GHz on every idle core.
        cores = read_core_clocks(collect=bench())
        for _label, megahertz in cores:
            self.assertAlmostEqual(megahertz, 3187.0 * 0.02, places=1)

    def test_instances_sort_by_processor_and_not_by_name(self):
        # PDH names them "0,0", "0,1", "0,10". As text, "0,10" lands between
        # "0,1" and "0,2", so a list ordered by name is not ordered by core.
        names = ["0,0", "0,1", "0,10", "0,2"]
        self.assertEqual(
            sorted(names, key=_instance_order),
            ["0,0", "0,1", "0,2", "0,10"],
        )

    def test_a_second_processor_group_does_not_collide(self):
        # The number inside the instance name restarts per group, so naming a
        # row after it would give two rows called CPU 0.
        rows = {
            "utility": [("0,0", 2.0), ("1,0", 4.0), ("_Total", 3.0)],
            "frequency": [("_Total", 3187.0)],
        }
        labels = [label for label, _mhz in read_core_clocks(collect=lambda: rows)]
        self.assertEqual(labels, ["CPU 0", "CPU 1"])

    def test_the_aggregates_are_not_listed_as_processors(self):
        cores = read_core_clocks(collect=bench())
        self.assertNotIn("_Total", [label for label, _mhz in cores])

    def test_no_sample_reports_no_processors(self):
        self.assertEqual(read_core_clocks(collect=lambda: {}), [])

    def test_the_row_list_does_not_depend_on_a_reading(self):
        # Rows are built at import, when these rate counters have only a
        # baseline and no value yet. Deriving the list from a reading gave an
        # empty one and the breakdown was simply absent.
        labels = core_labels()
        self.assertTrue(labels)
        self.assertEqual(labels[0], processor_label(0))


class FakeClock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now


class SampleSharingTest(unittest.TestCase):
    """Rate counters divide by the interval, so the interval must be real."""

    def _query(self):
        clock = FakeClock()
        query = cpu_clocks._Pdh(monotonic=clock)
        collected = []

        def collect():
            collected.append(clock.now)
            return {"performance": [("_Total", 173.8)],
                    "frequency": [("_Total", 3187.0)]}

        # Stand in for the whole PDH layer: the sharing rule is what is under
        # test, not the interop.
        query._open = lambda: True
        query._pdh = type("Pdh", (), {
            "PdhCollectQueryData": staticmethod(lambda _q: 0)})()
        query._array = lambda handle: handle
        query._counters = collect()
        collected.clear()
        return query, clock, collected

    def test_reads_inside_the_window_share_one_collection(self):
        query, clock, _ = self._query()
        query.collect()                      # primes
        clock.now += MIN_SAMPLE_INTERVAL_S   # first real sample
        first = query.collect()
        clock.now += MIN_SAMPLE_INTERVAL_S / 4
        self.assertIs(query.collect(), first)

    def test_priming_holds_the_next_collection_off(self):
        # The row after the priming one collected microseconds later and read
        # 5834 MHz. The window keeps maxima for the session, so that one
        # sample would have sat in the Max column until it was reset.
        query, clock, _ = self._query()
        self.assertEqual(query.collect(), {})
        clock.now += MIN_SAMPLE_INTERVAL_S / 100
        self.assertEqual(query.collect(), {})

    def test_a_read_past_the_window_takes_a_fresh_sample(self):
        query, clock, _ = self._query()
        query.collect()
        clock.now += MIN_SAMPLE_INTERVAL_S
        first = query.collect()
        clock.now += MIN_SAMPLE_INTERVAL_S * 2
        self.assertIsNot(query.collect(), first)


if __name__ == "__main__":
    unittest.main()
