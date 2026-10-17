//! Native AM5 rail routing. Never substitute a DIMM PMIC voltage for VDDIO.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::memo::Memo;
use crate::sensors::ite_superio::{IteSuperIoReader, TEMPERATURE_MAX_C, TEMPERATURE_MIN_C};
use crate::sensors::superio_lpc;
use crate::sensors::voltage_rails::{rail_by_key, validate_voltage};
use crate::system_identity::py_strip;
use crate::wmi_util;

static BOARD_IDENTITY: Memo<(String, String)> = Memo::new();

/// The board's maker and product, trimmed and upper-cased, or two empty
/// strings when WMI cannot say. Resolved once.
pub fn board_identity() -> (String, String) {
    BOARD_IDENTITY.get_or_init(|| {
        let identity = || -> Option<(String, String)> {
            let board = wmi_util::query("Win32_BaseBoard").ok()?.into_iter().next()?;
            // Python's str() of each property: a present-but-null one reads
            // "None", an absent one is the AttributeError that empties both.
            let field = |name: &str| -> Option<String> {
                let value = board.get(name)?;
                Some(value.to_text().unwrap_or_else(|| "None".into()))
            };
            let manufacturer = field("Manufacturer")?;
            let product = field("Product")?;
            Some((py_strip(&manufacturer).to_uppercase(), py_strip(&product).to_uppercase()))
        };
        identity().unwrap_or_default()
    })
}

pub const X870_TACHYON: (&str, &str) = ("GIGABYTE TECHNOLOGY CO., LTD.", "X870 AORUS TACHYON ICE");
pub const CHIP: &str = "IT8696E";

static READER: Memo<Arc<IteSuperIoReader>> = Memo::new();

/// The IT8696E reader for this board, detected once, or `None`.
fn board_reader() -> Option<Arc<IteSuperIoReader>> {
    if let Some(reader) = READER.get() {
        return Some(reader);
    }
    let mut candidate = IteSuperIoReader::new();
    if !candidate.detect() {
        return None;
    }
    let reader = Arc::new(candidate);
    READER.set(reader.clone());
    Some(reader)
}

fn is_tachyon(identity: &(String, String)) -> bool {
    identity.0 == X870_TACHYON.0 && identity.1 == X870_TACHYON.1
}

/// This board's CPU VDDIO, as `{rail key: volts}`; any other board's from
/// the Nuvoton Super I/O.
pub fn read_board_rails() -> BTreeMap<&'static str, f64> {
    if !is_tachyon(&board_identity()) {
        return superio_lpc::read_board_rails();
    }
    let Some(reader) = board_reader() else {
        return BTreeMap::new();
    };
    // IT8696E VIN6, register 0x26, 12 mV/count. This board returned
    // 118 counts (1.416 V), matching the user's same-boot VDDIO reading.
    // Gigabyte AM5 VIN6 mapping reference: LibreHardwareMonitor,
    // SuperIOHardware.cs (X670 AORUS family). Gate to this tested board;
    // no BIOS voltage sweep was performed to validate other boards.
    let Some(value) = reader.read_voltage(CHIP, 0x26, 1.0) else {
        return BTreeMap::new();
    };
    let Some(rail) = rail_by_key("vddio_mem") else {
        return BTreeMap::new();
    };
    match validate_voltage(rail, value) {
        Ok(volts) => BTreeMap::from([("vddio_mem", volts)]),
        Err(_) => BTreeMap::new(),
    }
}

// --- The rest of the IT8696E on this board: supplies, fans, one more probe.
//
// Every channel below was read on the X870 AORUS TACHYON ICE and decoded to
// what a reference tool showed for the same chip on the same boot. The dividers are
// ITE's usual ones for these inputs (12 mV per count, then the resistor
// ratio): +12V 168 x 12 mV x 6 = 12.096 V against the reference's 12.096, +3.3V
// 167 x x1.65 = 3.307 against 3.307, 3VSB, VBAT and AVCC3 x2 = 3.336, 3.048
// and 3.072 exactly, +5V 161 x x2.5 = 4.830 against 4.860 a moment apart.
//
// 0x24 (1.272 V) and 0x25 (1.116 V) are left out. The reference names them
// "iGPU VAXG" and "CPU VCCIN_AUX", Intel rails an AM5 board does not have,
// and naming them properly needs a BIOS change to see which one moves.

/// One board supply: (key, label, register, divider, plausible band in volts).
pub type BoardVoltage = (&'static str, &'static str, u8, f64, (f64, f64));

/// (key, label, register, divider, plausible band in volts)
pub const BOARD_VOLTAGES: &[BoardVoltage] = &[
    ("board_vcore", "Vcore (board)", 0x20, 1.0, (0.3, 1.8)),
    ("plus12v", "+12V", 0x22, 6.0, (10.8, 13.2)),
    ("plus5v", "+5V", 0x23, 2.5, (4.5, 5.5)),
    ("plus3v3", "+3.3V", 0x21, 1.65, (3.0, 3.6)),
    ("v3vsb", "3VSB", 0x27, 2.0, (3.0, 3.6)),
    ("vbat", "VBAT", 0x28, 2.0, (2.0, 3.6)),
    ("avcc3", "AVCC3", 0x2F, 2.0, (3.0, 3.6)),
];

/// Fan tachometers: a 16-bit count split over a low and a high register, fans
/// 4 to 6 in ITE's extended block. The headers are named in the order
/// the reference lists them. CPU Fan and CPU_OPT are the two with fans fitted on
/// the bench, and their counts decode to 1403 and 3648 RPM against its 1510 and
/// 3609; the other four read 0xFFFF, a header with nothing on it, which is why
/// which System Fan is which has not been seen here.
///
/// (key, label, low register, high register)
pub const BOARD_FANS: &[(&str, &str, u8, u8)] = &[
    ("cpu_fan", "CPU Fan", 0x0D, 0x18),
    ("sys_fan1", "System Fan 1", 0x0E, 0x19),
    ("sys_fan2", "System Fan 2", 0x0F, 0x1A),
    ("sys_fan3", "System Fan 3", 0x80, 0x81),
    ("cpu_opt", "CPU_OPT", 0x82, 0x83),
    ("fan6", "Fan 6", 0x84, 0x85),
];

/// Two thermistors: the chipset, 37 C beside the reference's "PCH" 37 C, and the
/// PCIe x16 slot, 34 C beside its 34 C.
///
/// (key, register)
pub const BOARD_TEMPERATURES: &[(&str, u8)] = &[("pch", 0x2A), ("pciex16", 0x2C)];

/// ITE's tachometer clock over two pulses a revolution.
pub const TACH_CLOCK: u32 = 1_350_000;

/// RPM from a tachometer count, or `None` for a header with no fan.
pub fn fan_rpm(low: Option<u8>, high: Option<u8>) -> Option<i64> {
    let (low, high) = (low?, high?);
    let count = ((high as u32) << 8) | low as u32;
    if count == 0 || count == 0xFFFF {
        return None;
    }
    Some(crate::pyfmt::round_int(TACH_CLOCK as f64 / (2 * count) as f64))
}

/// A thermistor reading in Celsius, or `None` for an unconnected probe.
fn board_temperature(raw: Option<u8>) -> Option<f64> {
    let raw = raw? as i32;
    if !(TEMPERATURE_MIN_C..=TEMPERATURE_MAX_C).contains(&raw) {
        return None;
    }
    Some(raw as f64)
}

/// This board's supplies, fans and slot probe, as one map. Read-only.
///
/// Keys are those of BOARD_VOLTAGES, BOARD_FANS and BOARD_TEMPERATURES; the
/// values are volts, RPM and degrees Celsius. A reading outside its band, and
/// a fan header with nothing on it, are left out rather than shown as a
/// number. Any other board gets an empty map: the map is this board's,
/// confirmed on it, and does not transfer.
pub fn read_board_monitor() -> BTreeMap<&'static str, f64> {
    if !is_tachyon(&board_identity()) {
        return BTreeMap::new();
    }
    let Some(reader) = board_reader() else {
        return BTreeMap::new();
    };
    // Every register this reads, in one hold of the bus.
    let mut registers: Vec<u8> = BOARD_VOLTAGES.iter().map(|v| v.2).collect();
    for &(_key, _label, low_register, high_register) in BOARD_FANS {
        registers.extend([low_register, high_register]);
    }
    registers.extend(BOARD_TEMPERATURES.iter().map(|(_key, register)| *register));
    let raw = reader.read_registers(CHIP, &registers);
    let step = reader.voltage_step(CHIP);
    let Some(step) = step.filter(|step| *step != 0.0) else {
        return BTreeMap::new();
    };
    if raw.is_empty() {
        return BTreeMap::new();
    }
    let get = |register: u8| raw.get(&register).copied().flatten();
    let mut found = BTreeMap::new();
    for &(key, _label, register, divider, (low, high)) in BOARD_VOLTAGES {
        let Some(value) = get(register) else {
            continue;
        };
        let volts = value as f64 * step * divider;
        if low <= volts && volts <= high {
            found.insert(key, volts);
        }
    }
    for &(key, _label, low_register, high_register) in BOARD_FANS {
        if let Some(rpm) = fan_rpm(get(low_register), get(high_register)) {
            found.insert(key, rpm as f64);
        }
    }
    for &(key, register) in BOARD_TEMPERATURES {
        if let Some(celsius) = board_temperature(get(register)) {
            found.insert(key, celsius);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::lowlevel_io::NamedMutex;
    use crate::sensors::ite_superio::test_support::FakeIte;
    use crate::sensors::ite_superio::{ISA_MUTEX_NAME, bus_mutex_name};
    use crate::wmi_util::{FakeWmi, WmiObject};

    const EC_BASE: u16 = 0x0A40;

    fn on_board(manufacturer: &str, product: &str) {
        FakeWmi::new()
            .class(
                "Win32_BaseBoard",
                vec![WmiObject::new().with("Manufacturer", manufacturer).with("Product", product)],
            )
            .install();
    }

    fn on_tachyon() {
        on_board("Gigabyte Technology Co., Ltd.", "X870 AORUS TACHYON ICE");
    }

    // The bench's IT8696E, as read beside a reference tool: raw counts per register.
    const BENCH_REGISTERS: &[(u8, u8)] = &[
        (0x20, 102), (0x21, 167), (0x22, 168), (0x23, 161), (0x27, 139), (0x28, 127),
        (0x2F, 128), (0x2A, 37), (0x2C, 34),
        (0x0D, 225), (0x18, 1), // CPU Fan: 481 counts
        (0x82, 185), (0x83, 0), // CPU_OPT: 185 counts
        (0x0E, 0xFF), (0x19, 0xFF), (0x0F, 0xFF), (0x1A, 0xFF),
        (0x80, 0xFF), (0x81, 0xFF), (0x84, 0xFF), (0x85, 0xFF),
    ];

    fn bench_chip(registers: &[(u8, u8)]) -> Arc<FakeIte> {
        FakeIte::it8696e(registers).install()
    }

    fn assert_close(actual: f64, expected: f64) {
        // assertAlmostEqual's default seven places.
        assert!((actual - expected).abs() < 0.5e-7, "{actual} != {expected}");
    }

    // == tests/test_x870_board_monitor.py ---------------------------------

    // -- FanRpmTest --------------------------------------------------------

    #[test]
    fn counts_decode_to_rpm() {
        assert_eq!(fan_rpm(Some(225), Some(1)), Some(1403));
        assert_eq!(fan_rpm(Some(185), Some(0)), Some(3649));
    }

    #[test]
    fn an_empty_header_has_no_rpm() {
        for (low, high) in [(Some(0xFF), Some(0xFF)), (Some(0), Some(0)), (None, Some(1)), (Some(1), None)] {
            assert_eq!(fan_rpm(low, high), None, "{low:?} {high:?}");
        }
    }

    // -- BoardMonitorTest --------------------------------------------------

    #[test]
    fn the_bench_decodes_to_the_reference_readings() {
        on_tachyon();
        bench_chip(BENCH_REGISTERS);
        let found = read_board_monitor();
        assert_close(found["plus12v"], 12.096);
        assert_close(found["plus3v3"], 3.3066);
        assert_close(found["plus5v"], 4.830);
        assert_close(found["v3vsb"], 3.336);
        assert_close(found["vbat"], 3.048);
        assert_close(found["avcc3"], 3.072);
        assert_close(found["board_vcore"], 1.224);
        assert_eq!((found["cpu_fan"], found["cpu_opt"]), (1403.0, 3649.0));
        assert_eq!((found["pch"], found["pciex16"]), (37.0, 34.0));
    }

    #[test]
    fn empty_headers_are_left_out() {
        on_tachyon();
        bench_chip(BENCH_REGISTERS);
        let found = read_board_monitor();
        for key in ["sys_fan1", "sys_fan2", "sys_fan3", "fan6"] {
            assert!(!found.contains_key(key), "{key}");
        }
    }

    #[test]
    fn a_reading_out_of_band_is_dropped() {
        // +12V reading zero is no reading.
        let registers: Vec<(u8, u8)> = BENCH_REGISTERS
            .iter()
            .map(|&(register, value)| (register, if register == 0x22 { 0 } else { value }))
            .collect();
        on_tachyon();
        bench_chip(&registers);
        let found = read_board_monitor();
        assert!(!found.contains_key("plus12v"));
        assert!(found.contains_key("plus5v"));
    }

    #[test]
    fn another_board_reads_nothing() {
        on_board("Gigabyte Technology Co., Ltd.", "X870E AORUS MASTER");
        let io = bench_chip(BENCH_REGISTERS);
        assert!(read_board_monitor().is_empty());
        assert!(io.state.lock().unwrap().writes.is_empty(), "the chip was touched");
    }

    #[test]
    fn the_whole_block_is_one_read() {
        // One pass over every register the monitor maps, each selected once.
        on_tachyon();
        let io = bench_chip(BENCH_REGISTERS);
        read_board_monitor();
        let mut expected: Vec<u8> = BOARD_VOLTAGES.iter().map(|v| v.2).collect();
        for &(_key, _label, low, high) in BOARD_FANS {
            expected.extend([low, high]);
        }
        expected.extend(BOARD_TEMPERATURES.iter().map(|t| t.1));
        assert_eq!(io.index_writes(EC_BASE), expected);
    }

    #[test]
    fn a_busy_bus_reads_nothing_rather_than_zeros() {
        on_tachyon();
        bench_chip(BENCH_REGISTERS);
        // Detected while the bus is free...
        assert!(!read_board_monitor().is_empty());
        // ...then another tool takes it for longer than the wait.
        let name = bus_mutex_name(ISA_MUTEX_NAME);
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let _bus = NamedMutex::acquire(&name, 1000).unwrap();
            held_tx.send(()).unwrap();
            release_rx.recv().ok();
        });
        held_rx.recv().unwrap();
        assert!(read_board_monitor().is_empty());
        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }

    #[test]
    fn the_two_intel_named_channels_are_not_claimed() {
        let registers: Vec<u8> = BOARD_VOLTAGES.iter().map(|v| v.2).collect();
        assert!(!registers.contains(&0x24));
        assert!(!registers.contains(&0x25));
    }

    // == tests/test_tachyon_readings.py -----------------------------------

    #[test]
    fn native_vddio() {
        // 118 counts at 12 mV is the 1.416 V read on the bench.
        on_tachyon();
        let io = bench_chip(&[(0x26, 118)]);
        let rails = read_board_rails();
        assert_eq!(rails.keys().copied().collect::<Vec<_>>(), vec!["vddio_mem"]);
        assert!((rails["vddio_mem"] - 1.416).abs() < 1e-9);
        assert_eq!(io.index_writes(EC_BASE), vec![0x26]);
        // The Python fed back None, 0, 3.3 and NaN. A register cannot hold NaN;
        // the others are a read that fails, a zero count, and the largest
        // count there is (3.06 V, as far out of the VDDIO band as 3.3 V).
        io.state.lock().unwrap().failing.push(EC_BASE);
        assert!(read_board_rails().is_empty());
        io.state.lock().unwrap().failing.clear();
        for raw in [0u8, 0xFF] {
            io.state.lock().unwrap().registers.insert(EC_BASE, [(0x26, raw)].into_iter().collect());
            assert!(read_board_rails().is_empty(), "{raw}");
        }
    }

    #[test]
    fn identity_is_upper_cased_and_trimmed() {
        on_board(" Gigabyte Technology Co., Ltd. ", "x870 aorus tachyon ice");
        assert_eq!(
            board_identity(),
            ("GIGABYTE TECHNOLOGY CO., LTD.".to_string(), "X870 AORUS TACHYON ICE".to_string())
        );
    }

    #[test]
    fn identity_is_stripped_the_way_python_strip_does() {
        // str.strip() takes the 0x1C-0x1F separators too, so Python 1.1.0
        // still recognises the Tachyon from a product name ending in one.
        on_board("Gigabyte Technology Co., Ltd.\x1c", "X870 AORUS TACHYON ICE\x1f");
        assert_eq!(
            board_identity(),
            ("GIGABYTE TECHNOLOGY CO., LTD.".to_string(), "X870 AORUS TACHYON ICE".to_string())
        );
    }
}
