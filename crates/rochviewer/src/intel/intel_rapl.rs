//! Intel package power, from the RAPL energy counters in MCHBAR.
//!
//! The counterpart of the AM5 profile's PPT/TDC/EDC: what the package is actually
//! drawing, which is the one thing the Intel Sensors tab could not say.
//!
//! RAPL is usually reached through MSRs, which this project cannot read -- the
//! bundled InpOut driver does port I/O and physical-memory mapping, and offers no
//! RDMSR. The same counters are also published in MCHBAR, which the timing
//! backend already reads, so no new transport is needed for this at all.
//!
//! Power is a rate, not a value. The registers are free-running 32-bit energy
//! accumulators, so a reading is the difference between two samples divided by
//! the time between them, and the first sample of a session can only be a
//! baseline.
//!
//! SAFETY - read-only. Physical reads through the same MCHBAR window the timing
//! backend already uses. Nothing here writes.

use std::sync::OnceLock;
use std::time::Instant;

use crate::hardware::read::read_dword;
use crate::memo::MemoMap;

pub const MCHBAR: u64 = 0xFEDC_0000;

/// Unit register: power in bits [3:0], energy in [12:8], time in [19:16], each
/// the negative power of two the domain is counted in.
///
/// Reads 0x000A0E03 on the Z790 bench: power 1/8 W, energy 1/2^14 J = 61.035 uJ,
/// time 1/2^10 s. That energy unit is the standard client value, and it is what
/// makes the counters below decode to the right watts rather than a number that
/// merely moves in the right direction.
pub const RAPL_POWER_UNIT: u64 = 0x5938;

/// Energy-status accumulators.
///
/// Confirmed on ASUS ROG MAXIMUS Z790 APEX + i9-14900KS against HWiNFO reading
/// the same machine, and then through an idle -> all-core load -> idle cycle,
/// because a counter that merely increases proves nothing on its own:
///
/// ```text
///              idle      loaded    recovered   HWiNFO idle
///   0x593C     43.8 W    193.8 W   43.3 W      CPU Package Power 42.7 W
///   0x5928     34.2 W    183.5 W   33.0 W      IA Cores Power    32.6 W
/// ```
///
/// The pair corroborate each other as well as HWiNFO: package minus cores is
/// 9.6 W at idle and 10.3 W loaded, against HWiNFO's System Agent 8.9 W plus
/// Rest-of-Chip 0.4 W. A mis-assigned counter would not hold that difference
/// across a 4x swing in draw.
pub const ENERGY_STATUS: &[(&str, u64)] = &[("package", 0x593C), ("cores", 0x5928)];

// A rate outside this band is a failed read or a counter that wrapped more
// than once, not a package draw. Desktop parts do not sustain a kilowatt.
pub const POWER_MIN_W: f64 = 0.0;
pub const POWER_MAX_W: f64 = 1000.0;

/// Past this the previous sample is too old to subtract from. The counter is
/// 32 bits, so at a few hundred watts it wraps in about twenty minutes and a
/// stale baseline would give one confident, wrong reading on the way back.
pub const MAX_SAMPLE_AGE_S: f64 = 30.0;

/// domain -> (raw counter, timestamp) from the previous read.
static LAST: MemoMap<&'static str, (u32, f64)> = MemoMap::new();

/// The RAPL units, each as the size of one count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RaplUnits {
    /// Watts per count.
    pub power: f64,
    /// Joules per count.
    pub energy: f64,
    /// Seconds per count.
    pub time: f64,
}

/// Return the power, energy and time units (W, J, s per count).
pub fn decode_units(raw: u32) -> RaplUnits {
    RaplUnits {
        power: 1.0 / (1u64 << (raw & 0x0F)) as f64,
        energy: 1.0 / (1u64 << ((raw >> 8) & 0x1F)) as f64,
        time: 1.0 / (1u64 << ((raw >> 16) & 0x0F)) as f64,
    }
}

/// Watts between two samples of a free-running 32-bit counter.
///
/// The subtraction is masked, so one wrap between samples is handled by the
/// arithmetic rather than needing to be detected.
pub fn energy_rate(previous: u32, current: u32, seconds: f64, energy_unit: f64) -> Option<f64> {
    if seconds <= 0.0 {
        return None;
    }
    let delta = current.wrapping_sub(previous);
    Some(delta as f64 * energy_unit / seconds)
}

/// Return the reading, or `None` when it cannot be a package draw.
pub fn validate_power(watts: f64) -> Option<f64> {
    (POWER_MIN_W..=POWER_MAX_W).contains(&watts).then_some(watts)
}

/// Seconds on a clock that only moves forward (Python's `time.monotonic`).
fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Return watts for one RAPL domain, or `None`.
///
/// `None` on the first call of a session: one sample of an accumulator is a
/// baseline, not a rate. The row shows nothing for one tick rather than
/// showing a total dressed up as a power.
pub fn read_power(domain: &str) -> Option<f64> {
    read_power_with_clock(domain, monotonic)
}

/// [`read_power`] on a given clock, which is read after the registers.
fn read_power_with_clock(domain: &str, clock: impl Fn() -> f64) -> Option<f64> {
    let &(domain, offset) = ENERGY_STATUS.iter().find(|(name, _)| *name == domain)?;
    let units = read_dword(MCHBAR + RAPL_POWER_UNIT);
    let current = read_dword(MCHBAR + offset);
    let (Some(units), Some(current)) = (units, current) else {
        return None;
    };
    if units == 0x0000_0000 || units == 0xFFFF_FFFF {
        return None;
    }

    let now = clock();
    let previous = LAST.get(&domain);
    LAST.insert(domain, (current, now));
    let (last_raw, last_time) = previous?;
    let elapsed = now - last_time;
    if elapsed <= 0.0 || elapsed > MAX_SAMPLE_AGE_S {
        return None;
    }

    validate_power(energy_rate(last_raw, current, elapsed, decode_units(units).energy)?)
}

/// Format one domain for a table row, or `None` when unavailable.
pub fn power_text(domain: &str) -> Option<String> {
    read_power(domain).map(|watts| format!("{watts:.1} W"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::FakeHardware;
    use std::cell::Cell;

    // What the Z790 bench reports: power 1/8 W, energy 1/2^14 J, time 1/2^10 s.
    const BENCH_UNITS: u32 = 0x000A_0E03;
    const ENERGY_UNIT: f64 = 1.0 / (1u64 << 14) as f64;

    fn offset(domain: &str) -> u64 {
        ENERGY_STATUS.iter().find(|(d, _)| *d == domain).unwrap().1
    }

    fn assert_close(actual: f64, expected: f64, places: i32) {
        assert!(
            (actual - expected).abs() < 0.5 * 10f64.powi(-places),
            "{actual} != {expected} to {places} places"
        );
    }

    // -- UnitDecodeTest ----------------------------------------------------

    #[test]
    fn the_bench_unit_register() {
        let units = decode_units(BENCH_UNITS);
        assert_close(units.power, 0.125, 7);
        assert_close(units.energy, ENERGY_UNIT, 7);
        assert_close(units.time, 1.0 / 1024.0, 7);
    }

    #[test]
    fn each_field_is_read_from_its_own_bits() {
        let units = decode_units(0x000F_1F0F);
        assert_close(units.power, 1.0 / (1u64 << 15) as f64, 7);
        assert_close(units.energy, 1.0 / (1u64 << 31) as f64, 7);
        assert_close(units.time, 1.0 / (1u64 << 15) as f64, 7);
    }

    // -- EnergyRateTest ----------------------------------------------------

    #[test]
    fn the_bench_idle_reading() {
        // 716,957 counts a second at 61.035 uJ is the 43.8 W measured against
        // HWiNFO's 42.7 W.
        assert_close(energy_rate(0, 716957, 1.0, ENERGY_UNIT).unwrap(), 43.8, 1);
    }

    #[test]
    fn a_wrapped_counter_is_still_a_rate() {
        // 32 bits at a few hundred watts wraps in about twenty minutes, so
        // this is a normal event and not an error.
        // 0xFFFFFF00 -> 0x000000FF is 0x100 counts to the wrap plus 0xFF after
        // it, so 511, not the 255 the end value alone suggests.
        let watts = energy_rate(0xFFFF_FF00, 0x0000_00FF, 1.0, ENERGY_UNIT).unwrap();
        assert_close(watts, 511.0 * ENERGY_UNIT, 6);
        assert!(watts > 0.0);
    }

    #[test]
    fn no_elapsed_time_is_not_a_rate() {
        assert_eq!(energy_rate(0, 1000, 0.0, ENERGY_UNIT), None);
    }

    #[test]
    fn an_impossible_draw_is_dropped() {
        assert_eq!(validate_power(5000.0), None);
        assert_eq!(validate_power(-1.0), None);
        // The Python's None case: NaN is the one unreadable float.
        assert_eq!(validate_power(f64::NAN), None);
        assert_eq!(validate_power(43.8), Some(43.8));
    }

    // -- ReadPowerTest -----------------------------------------------------

    /// MCHBAR with the bench unit register and both energy counters.
    fn bench(package: u32, cores: u32) -> std::sync::Arc<FakeHardware> {
        FakeHardware::new()
            .with_dword(MCHBAR + RAPL_POWER_UNIT, BENCH_UNITS)
            .with_dword(MCHBAR + offset("package"), package)
            .with_dword(MCHBAR + offset("cores"), cores)
            .install()
    }

    #[test]
    fn the_first_sample_is_a_baseline_not_a_reading() {
        bench(1000, 0);
        assert_eq!(read_power_with_clock("package", || 100.0), None);
    }

    #[test]
    fn the_second_sample_gives_watts() {
        let hw = bench(0, 0);
        let clock = Cell::new(100.0);
        read_power_with_clock("package", || clock.get());
        hw.set_dword(MCHBAR + offset("package"), 716957);
        clock.set(101.0);
        let watts = read_power_with_clock("package", || clock.get()).unwrap();
        assert_close(watts, 43.8, 1);
    }

    #[test]
    fn a_stale_baseline_is_refused() {
        // The counter wraps in minutes; subtracting from a sample taken before
        // the window was closed would give one confident wrong number.
        let hw = bench(0, 0);
        let clock = Cell::new(100.0);
        read_power_with_clock("package", || clock.get());
        hw.set_dword(MCHBAR + offset("package"), 716957);
        clock.set(100.0 + MAX_SAMPLE_AGE_S + 1.0);
        assert_eq!(read_power_with_clock("package", || clock.get()), None);
    }

    #[test]
    fn an_unreadable_register_reports_nothing() {
        FakeHardware::new().install();
        assert_eq!(read_power_with_clock("package", || 1.0), None);
        let mut all_ones = FakeHardware::new();
        all_ones.unmapped_reads_ones = true;
        all_ones.install();
        assert_eq!(read_power_with_clock("package", || 1.0), None);
    }

    #[test]
    fn an_unknown_domain_reports_nothing() {
        bench(0, 0);
        assert_eq!(read_power_with_clock("gpu", || 1.0), None);
    }

    #[test]
    fn each_domain_keeps_its_own_baseline() {
        // Sharing one would make whichever row polled second read the other's
        // delta.
        let hw = bench(0, 0);
        let clock = Cell::new(100.0);
        read_power_with_clock("package", || clock.get());
        read_power_with_clock("cores", || clock.get());
        hw.set_dword(MCHBAR + offset("package"), 716957);
        hw.set_dword(MCHBAR + offset("cores"), 559000);
        clock.set(101.0);
        assert_close(read_power_with_clock("package", || clock.get()).unwrap(), 43.8, 1);
        assert_close(read_power_with_clock("cores", || clock.get()).unwrap(), 34.1, 1);
    }

    #[test]
    fn power_text_is_nothing_on_the_baseline() {
        bench(0, 0);
        assert_eq!(power_text("package"), None);
    }
}
