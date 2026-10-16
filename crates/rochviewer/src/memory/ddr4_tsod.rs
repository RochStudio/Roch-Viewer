//! DDR4 on-module temperature sensors (JEDEC JC-42.4 / TSE2004av).
//!
//! A DDR4 module may carry a thermal sensor next to its SPD EEPROM. It is
//! optional: plenty of desktop kits leave the part off the board, and a module
//! without one simply does not answer. That is why every reading here is
//! either a value the sensor returned or nothing at all - there is no modelled
//! or inferred temperature.
//!
//! The sensor lives at 0x18 + slot on the SMBus, one address per slot, and its
//! ambient-temperature register is a single 16-bit word. DDR5 moved this
//! function into the SPD5 hub, so that generation is read elsewhere; this
//! module is only for DDR4.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::ddr5_telemetry::{ReaderFactory, SmbusError, SmbusReader, monotonic};
use crate::memo::Memo;

// Ambient temperature. The other JC-42.4 registers are capability, config and
// the three limit registers, none of which this project reads.
pub const AMBIENT_TEMPERATURE_REGISTER: u8 = 0x05;

// The reading occupies the low 13 bits, in 1/16 degree steps, two's complement
// with the sign at bit 12. Bits 15-13 are the TCRIT/TUPPER/TLOWER alarm flags
// and are not part of the number.
pub const TEMPERATURE_MASK: i32 = 0x1FFF;
pub const SIGN_BIT: i32 = 0x1000;
pub const SIGN_OFFSET: i32 = 0x2000;
pub const STEP_CELSIUS: f64 = 0.0625;

// What the part can physically report. A decode outside this is not a cold or
// hot DIMM, it is a failed transfer, and is dropped rather than displayed.
pub const MIN_CELSIUS: f64 = -40.0;
pub const MAX_CELSIUS: f64 = 125.0;

// Slot index -> channel. Intel desktop boards fill the first two slot positions
// from the first channel and the next two from the second.
pub const SLOTS_PER_CHANNEL: i32 = 2;
pub const CHANNEL_LABELS: [&str; 2] = ["a", "b"];

/// Decode the ambient-temperature register, or `None` when it cannot be one.
///
/// The sensor answers most-significant byte first, which is the opposite of
/// the SMBus word convention, so the caller passes the bytes in wire order.
/// A byte that did not read (`None`) decodes to nothing.
pub fn decode_temperature(high_byte: impl Into<Option<u8>>, low_byte: impl Into<Option<u8>>) -> Option<f64> {
    let (high, low) = (high_byte.into()?, low_byte.into()?);
    let raw = ((high as i32) << 8) | low as i32;

    let mut value = raw & TEMPERATURE_MASK;
    if value & SIGN_BIT != 0 {
        value -= SIGN_OFFSET;
    }
    let celsius = value as f64 * STEP_CELSIUS;
    if !(MIN_CELSIUS..=MAX_CELSIUS).contains(&celsius) {
        return None;
    }
    Some(celsius)
}

/// The channel letter a sensor address belongs to, or `None`.
pub fn channel_for_address(address: u8, base_address: u8) -> Option<&'static str> {
    let index = address as i32 - base_address as i32;
    if index < 0 {
        return None;
    }
    let channel = (index / SLOTS_PER_CHANNEL) as usize;
    CHANNEL_LABELS.get(channel).copied()
}

/// `{channel letter: celsius}`.
pub type ChannelTemperatures = BTreeMap<&'static str, f64>;

// A temperature moves slowly and every read costs a bus transaction taken
// under a mutex that HWiNFO and friends also want, so a reading is reused for
// a moment rather than re-fetched once per row per refresh.
pub const CACHE_SECONDS: f64 = 0.75;

#[derive(Clone, Default)]
struct TsodState {
    reader: Option<Arc<dyn SmbusReader>>,
    addresses: Option<Vec<u8>>,
    cached: (ChannelTemperatures, f64),
    // Highest reading seen per channel since the process started. A memory
    // test runs for hours and the peak is what decides whether the kit is
    // cooled well enough; the instantaneous value at the moment someone
    // glances at the window is not that number.
    //
    // Not what the row displays -- see temperature_text. The telemetry window
    // tracks its own maximum per row and clears it on Reset Stats; this one
    // spans the process and survives that button, which is a different
    // question and the reason it is still kept. A caller wanting the one on
    // screen should read the window's, not this.
    peaks: ChannelTemperatures,
}

static STATE_LOCK: Mutex<()> = Mutex::new(());
static STATE: Memo<TsodState> = Memo::new();

fn state() -> TsodState {
    STATE.get().unwrap_or_default()
}

/// Build the reader once. Locating the controller cannot change at runtime.
fn get_reader(
    state: &mut TsodState,
    reader_factory: Option<ReaderFactory>,
) -> Result<Arc<dyn SmbusReader>, SmbusError> {
    if let Some(factory) = reader_factory {
        return factory();
    }
    if state.reader.is_none() {
        state.reader = Some(crate::intel::intel_pch_smbus::new_reader()?);
    }
    Ok(state.reader.clone().expect("built above"))
}

/// Scan for sensors once. Modules cannot be added without a power cycle.
fn get_addresses(state: &mut TsodState, reader: &dyn SmbusReader, rescan: bool) -> Result<Vec<u8>, SmbusError> {
    if rescan || state.addresses.is_none() {
        let found = reader.responding_addresses(AMBIENT_TEMPERATURE_REGISTER)?;
        if rescan {
            return Ok(found);
        }
        state.addresses = Some(found);
    }
    Ok(state.addresses.clone().unwrap_or_default())
}

/// Drop the cached controller, sensor list, reading and peaks.
pub fn reset_cache() {
    let _held = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    STATE.set(TsodState::default());
}

/// `{channel letter: highest celsius seen}`.
pub fn peak_temperatures() -> ChannelTemperatures {
    state().peaks
}

/// `{channel letter: celsius}` for every sensor that answered.
///
/// Fail-closed like the other privileged transports: a slot that does not
/// answer, or that answers with something outside the part's range, is left
/// out of the result rather than reported as a number. When a channel holds
/// two populated slots, the hotter one is kept - that is the module the
/// airflow is failing and the one worth watching while tuning.
pub fn read_dimm_temperatures(reader_factory: Option<ReaderFactory>) -> ChannelTemperatures {
    read_dimm_temperatures_at(reader_factory, &monotonic)
}

fn read_dimm_temperatures_at(
    reader_factory: Option<ReaderFactory>,
    monotonic: &dyn Fn() -> f64,
) -> ChannelTemperatures {
    let injected = reader_factory.is_some();

    if !injected {
        let (values, stamp) = state().cached;
        let now = monotonic();
        if !values.is_empty() && now - stamp < CACHE_SECONDS {
            return values;
        }
    }

    let (reader, addresses) = {
        let _held = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let mut current = state();
        let resolved = get_reader(&mut current, reader_factory)
            .and_then(|reader| get_addresses(&mut current, reader.as_ref(), injected).map(|found| (reader, found)));
        STATE.set(current);
        match resolved {
            Ok(resolved) => resolved,
            Err(_) => return ChannelTemperatures::new(),
        }
    };

    let mut temperatures = ChannelTemperatures::new();
    for address in addresses {
        let Some(channel) = channel_for_address(address, 0x18) else {
            continue;
        };
        let Ok((high, low)) = reader.read_word_bytes(address, AMBIENT_TEMPERATURE_REGISTER) else {
            continue;
        };
        let Some(celsius) = decode_temperature(high, low) else {
            continue;
        };
        let previous = temperatures.get(channel).copied();
        if previous.is_none_or(|previous| celsius > previous) {
            temperatures.insert(channel, celsius);
        }
    }

    let _held = STATE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut current = state();
    for (channel, celsius) in &temperatures {
        if *celsius > current.peaks.get(channel).copied().unwrap_or(f64::NEG_INFINITY) {
            current.peaks.insert(channel, *celsius);
        }
    }

    if !injected && !temperatures.is_empty() {
        current.cached = (temperatures.clone(), monotonic());
    }
    STATE.set(current);
    temperatures
}

/// Format one channel's DIMM temperature, or `None`.
///
/// The reading alone. It used to trail its own peak -- "29.2 °C  (max 99.9)"
/// -- because these rows lived on a tab, and a tab shows one instant, so the
/// peak had nowhere else to go.
///
/// They live in the telemetry window now, which gives every row a Max column
/// tracking exactly that, and a button to reset it. Carrying the peak here as
/// well was wrong twice over: nineteen characters in a column sized for
/// seven, so the text overflowed across the Min and Max cells beside it; and
/// a second maximum, kept in this module, that Reset Stats could not clear
/// and that would drift from the one on screen the moment it was pressed.
pub fn temperature_text(channel: &str, reader_factory: Option<ReaderFactory>) -> Option<String> {
    read_dimm_temperatures(reader_factory).get(channel).map(|celsius| format!("{celsius:.1} °C"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Answers for a chosen set of sensor addresses, in wire order.
    struct FakeReader {
        words: BTreeMap<u8, (u8, u8)>,
        reads: Mutex<Vec<(u8, u8)>>,
    }

    impl FakeReader {
        fn new(words: &[(u8, (u8, u8))]) -> Arc<Self> {
            Arc::new(FakeReader { words: words.iter().copied().collect(), reads: Mutex::new(Vec::new()) })
        }

        fn read_count(&self) -> usize {
            self.reads.lock().unwrap().len()
        }
    }

    impl SmbusReader for FakeReader {
        fn is_driver_open(&self) -> bool {
            true
        }

        fn responding_addresses(&self, _register: u8) -> Result<Vec<u8>, SmbusError> {
            Ok(self.words.keys().copied().collect())
        }

        fn read_word_bytes(&self, address: u8, register: u8) -> Result<(u8, u8), SmbusError> {
            self.reads.lock().unwrap().push((address, register));
            self.words.get(&address).copied().ok_or_else(|| SmbusError::Bus(format!("no sensor at 0x{address:02X}")))
        }
    }

    fn factory(reader: &Arc<FakeReader>) -> impl Fn() -> Result<Arc<dyn SmbusReader>, SmbusError> + use<> {
        let reader = reader.clone();
        move || Ok(reader.clone() as Arc<dyn SmbusReader>)
    }

    fn read(reader: &Arc<FakeReader>) -> ChannelTemperatures {
        read_dimm_temperatures(Some(&factory(reader)))
    }

    fn temps(pairs: &[(&'static str, f64)]) -> ChannelTemperatures {
        pairs.iter().copied().collect()
    }

    // --- DecodeTest: values captured from the Z790 target, plus the
    // boundary cases.

    #[test]
    fn the_reading_captured_on_channel_a() {
        // 0xC1E8: alarm flags in the top bits, 0x1E8 = 488 sixteenths.
        assert_eq!(decode_temperature(0xC1, 0xE8), Some(30.5));
    }

    #[test]
    fn the_reading_captured_on_channel_b() {
        assert_eq!(decode_temperature(0xC2, 0x00), Some(32.0));
    }

    #[test]
    fn alarm_flags_are_not_part_of_the_number() {
        // Same temperature, every alarm flag combination.
        for flags in [0x00, 0x20, 0x40, 0x60, 0x80, 0xA0, 0xC0, 0xE0] {
            assert_eq!(decode_temperature(0x01 | flags, 0xE8), Some(30.5));
        }
    }

    #[test]
    fn a_sixteenth_of_a_degree_resolves() {
        assert_eq!(decode_temperature(0x00, 0x01), Some(0.0625));
    }

    #[test]
    fn a_negative_temperature_decodes() {
        // -0.25 C is 0x1FFC in the 13-bit two's complement field.
        assert_eq!(decode_temperature(0x1F, 0xFC), Some(-0.25));
    }

    #[test]
    fn minus_one_degree_decodes() {
        assert_eq!(decode_temperature(0x1F, 0xF0), Some(-1.0));
    }

    #[test]
    fn zero_decodes() {
        assert_eq!(decode_temperature(0x00, 0x00), Some(0.0));
    }

    #[test]
    fn a_decode_below_the_parts_range_is_dropped() {
        // 0x1000 is the sign bit alone: -256 C, which no sensor reports.
        assert_eq!(decode_temperature(0x10, 0x00), None);
    }

    #[test]
    fn a_decode_above_the_parts_range_is_dropped() {
        // 0x0FFF is +255.9375 C.
        assert_eq!(decode_temperature(0x0F, 0xFF), None);
    }

    #[test]
    fn unreadable_bytes_are_dropped() {
        assert_eq!(decode_temperature(None, 0x00), None);
        assert_eq!(decode_temperature(0xC1, None), None);
    }

    #[test]
    fn byte_order_is_most_significant_first() {
        // Swapping the captured bytes must not yield the same temperature.
        assert_ne!(decode_temperature(0xC1, 0xE8), decode_temperature(0xE8, 0xC1));
    }

    // --- ChannelMappingTest

    #[test]
    fn the_first_two_slots_are_channel_a() {
        assert_eq!(channel_for_address(0x18, 0x18), Some("a"));
        assert_eq!(channel_for_address(0x19, 0x18), Some("a"));
    }

    #[test]
    fn the_next_two_slots_are_channel_b() {
        assert_eq!(channel_for_address(0x1A, 0x18), Some("b"));
        assert_eq!(channel_for_address(0x1B, 0x18), Some("b"));
    }

    #[test]
    fn addresses_beyond_the_mapped_slots_are_unmapped() {
        for address in [0x1C, 0x1D, 0x1E, 0x1F] {
            assert_eq!(channel_for_address(address, 0x18), None);
        }
    }

    #[test]
    fn an_address_below_the_range_is_unmapped() {
        assert_eq!(channel_for_address(0x17, 0x18), None);
    }

    // --- ReadTest (each test thread starts with an empty state)

    #[test]
    fn the_captured_layout_maps_to_both_channels() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8)), (0x1B, (0xC2, 0x00))]);
        assert_eq!(read(&reader), temps(&[("a", 30.5), ("b", 32.0)]));
    }

    #[test]
    fn the_hotter_module_in_a_channel_wins() {
        let reader = FakeReader::new(&[(0x18, (0xC1, 0xE8)), (0x19, (0xC2, 0x00))]);
        assert_eq!(read(&reader), temps(&[("a", 32.0)]));
    }

    #[test]
    fn a_board_with_no_sensors_reports_nothing() {
        let reader = FakeReader::new(&[]);
        assert_eq!(read(&reader), temps(&[]));
    }

    #[test]
    fn an_implausible_reading_is_left_out() {
        let reader = FakeReader::new(&[(0x19, (0x0F, 0xFF))]);
        assert_eq!(read(&reader), temps(&[]));
    }

    #[test]
    fn a_failure_to_reach_the_bus_reports_nothing() {
        let explode = || -> Result<Arc<dyn SmbusReader>, SmbusError> { Err(SmbusError::Bus("no controller".into())) };
        assert_eq!(read_dimm_temperatures(Some(&explode)), temps(&[]));
    }

    #[test]
    fn formatting_carries_one_decimal_and_a_unit() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        assert_eq!(temperature_text("a", Some(&factory(&reader))).as_deref(), Some("30.5 °C"));
    }

    #[test]
    fn formatting_an_absent_channel_reports_nothing() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        assert_eq!(temperature_text("b", Some(&factory(&reader))), None);
    }

    // --- PeakTest: the peak is what a memory test is judged on, not the
    // current reading.

    #[test]
    fn the_peak_starts_empty() {
        reset_cache();
        assert_eq!(peak_temperatures(), temps(&[]));
    }

    #[test]
    fn the_first_reading_becomes_the_peak() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&reader);
        assert_eq!(peak_temperatures(), temps(&[("a", 30.5)]));
    }

    #[test]
    fn a_hotter_reading_raises_the_peak() {
        let hot = FakeReader::new(&[(0x19, (0xC2, 0x00))]);
        let cool = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&cool);
        read(&hot);
        assert_eq!(peak_temperatures()["a"], 32.0);
    }

    #[test]
    fn a_cooler_reading_does_not_lower_the_peak() {
        let hot = FakeReader::new(&[(0x19, (0xC2, 0x00))]);
        let cool = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&hot);
        read(&cool);
        assert_eq!(peak_temperatures()["a"], 32.0);
    }

    #[test]
    fn each_channel_keeps_its_own_peak() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8)), (0x1B, (0xC2, 0x00))]);
        read(&reader);
        assert_eq!(peak_temperatures(), temps(&[("a", 30.5), ("b", 32.0)]));
    }

    #[test]
    fn the_row_shows_the_reading_and_not_the_peak() {
        // It used to append "(max 32.0)". These rows are in the telemetry
        // window now, which has a Max column of its own, so the suffix was
        // both redundant and nineteen characters wide in a column sized for
        // seven -- it overflowed across the Min and Max cells beside it.
        let hot = FakeReader::new(&[(0x19, (0xC2, 0x00))]);
        let cool = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&hot);
        assert_eq!(temperature_text("a", Some(&factory(&cool))).as_deref(), Some("30.5 °C"));
    }

    #[test]
    fn the_row_is_the_same_width_whatever_the_peak_is() {
        // The overflow only appeared once a peak had been recorded, so the
        // row was fine until the kit warmed up and then broke the table.
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        let plain = temperature_text("a", Some(&factory(&reader)));
        let mut current = state();
        current.peaks.insert("a", 99.9);
        STATE.set(current);
        assert_eq!(temperature_text("a", Some(&factory(&reader))), plain);
    }

    #[test]
    fn the_peak_is_still_tracked_for_callers_that_want_it() {
        // Dropping it from the row does not drop it from the module: this one
        // spans the process and survives Reset Stats, which the window's
        // maximum does not.
        let hot = FakeReader::new(&[(0x19, (0xC2, 0x00))]);
        let cool = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&hot);
        read(&cool);
        assert_eq!(peak_temperatures()["a"], 32.0);
    }

    #[test]
    fn resetting_clears_the_peak() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&reader);
        reset_cache();
        assert_eq!(peak_temperatures(), temps(&[]));
    }

    #[test]
    fn the_peak_is_a_copy_that_callers_cannot_corrupt() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&reader);
        peak_temperatures().insert("a", 999.0);
        assert_eq!(peak_temperatures()["a"], 30.5);
    }

    // --- CacheTest: a row must not cost a bus transaction every time it is
    // drawn.

    fn install(reader: &Arc<FakeReader>, addresses: &[u8]) {
        let mut current = state();
        current.reader = Some(reader.clone());
        current.addresses = Some(addresses.to_vec());
        STATE.set(current);
    }

    fn clock(ticks: &[f64]) -> impl Fn() -> f64 + use<> {
        let ticks = Mutex::new(ticks.iter().copied().collect::<VecDeque<f64>>());
        move || ticks.lock().unwrap().pop_front().expect("clock ran out")
    }

    #[test]
    fn a_repeat_read_inside_the_window_does_not_touch_the_bus() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        let clock = clock(&[0.0, 0.0, 0.1, 0.2]);
        install(&reader, &[0x19]);

        let first = read_dimm_temperatures_at(None, &clock);
        assert_eq!(first, temps(&[("a", 30.5)]));
        let before = reader.read_count();

        let second = read_dimm_temperatures_at(None, &clock);
        assert_eq!(second, temps(&[("a", 30.5)]));
        assert_eq!(reader.read_count(), before);
    }

    #[test]
    fn the_bus_is_read_again_once_the_window_passes() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        let clock = clock(&[0.0, 0.0, 10.0, 10.0]);
        install(&reader, &[0x19]);

        read_dimm_temperatures_at(None, &clock);
        let before = reader.read_count();
        read_dimm_temperatures_at(None, &clock);
        assert!(reader.read_count() > before);
    }

    #[test]
    fn an_injected_reader_bypasses_the_cache() {
        let reader = FakeReader::new(&[(0x19, (0xC1, 0xE8))]);
        read(&reader);
        let before = reader.read_count();
        read(&reader);
        assert!(reader.read_count() > before);
    }
}
