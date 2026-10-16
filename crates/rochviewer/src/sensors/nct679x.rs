//! Read-only NCT679x hardware-monitor access.
//!
//! Companion to [`superio_lpc`](super::superio_lpc), which speaks the other
//! Nuvoton sensor model. The two are not variants of one protocol; they are
//! different register files reached different ways, and the distinction is not
//! cosmetic:
//!
//! ```text
//!   NCT668x (superio_lpc)   an embedded-controller window at the LDN 0x0B base,
//!                           addressed page/index/data at base+4/5/6, sensors in
//!                           flat 16-bit blocks at 0x100 and 0x120.
//!
//!   NCT679x (this module)   a bank-switched register file at the same base,
//!                           addressed index/data at base+5/6 after selecting a
//!                           bank through register 0x4E, sensors as single bytes
//!                           scattered across banks 0-7.
//! ```
//!
//! Addressing one with the other's protocol does not fault. It returns 0xFFFF and
//! repeated bytes, and 0xFFFF passes a temperature band as -0.004 C, which is how
//! an ASUS ROG MAXIMUS Z790 APEX came to report a 3.7 C CPU. See EC_WINDOW_CHIPS
//! in superio_lpc for the gate that stops that.
//!
//! SAFETY -- this module reads sensors and nothing else, on the same terms as
//! superio_lpc:
//!
//!   * The only writes are the ones the read path requires: the vendor unlock
//!     sequence, the logical-device selector, and the bank/index registers that
//!     address a sensor byte. Every one of those selects *what to read*.
//!   * There is no write to any sensor, fan, PWM or configuration value, and no
//!     primitive that could perform one. This chip also drives fan control, so
//!     that capability is deliberately absent rather than merely unused.
//!   * Config mode is always exited, including on error.
//!   * Access is serialised on the named mutex LibreHardwareMonitor and HWiNFO
//!     take for ISA/LPC access, so a concurrent monitor cannot land between the
//!     bank select and the read.

use std::fmt;
use std::sync::Mutex;

use crate::hardware::lowlevel_io::{DEFAULT_MUTEX_TIMEOUT_MS, NamedMutex};
use crate::hardware::{HwError, HwResult, hw};
use crate::sensors::superio_lpc::{
    CONFIG_PORTS, ISA_MUTEX_NAME, LDN_EC_SPACE, NUVOTON_LOCK_BYTE, NUVOTON_UNLOCK_BYTE,
    REG_BASE_ADDRESS_HIGH, REG_BASE_ADDRESS_LOW, REG_CHIP_ID_HIGH, REG_CHIP_ID_LOW,
    REG_LOGICAL_DEVICE,
};

// Offsets inside the hardware-monitor window.
pub const HWM_INDEX_OFFSET: u16 = 0x05;
pub const HWM_DATA_OFFSET: u16 = 0x06;

/// Writing this register on the index port selects which bank the next index
/// refers to. A sensor address here is (bank << 8) | register.
pub const BANK_SELECT_REGISTER: u8 = 0x4E;

/// Chip IDs using this register model.
///
/// 0xD42B is what the ASUS ROG MAXIMUS Z790 APEX answers on config port 0x2E.
/// HWiNFO names that same chip "Nuvoton NCT6798D" in its sensor tree, which is
/// where the name here comes from; nothing in this project decodes a model name
/// out of the ID itself.
pub const NCT679X_CHIP_IDS: &[(u16, &str)] = &[(0xD42B, "NCT6798D")];

/// Voltage LSB at the ADC. Every channel in the 0x480 block is a byte, but the
/// volts each count is worth depends on the divider in front of that channel,
/// which is the board vendor's choice: 8 mV undivided, 16 mV behind a 2:1, and
/// 18 mV on the memory-controller channel. So a rail records its own step
/// rather than inheriting this one, and this value is only the starting point a
/// step is derived from.
pub const VOLTAGE_STEP_VOLTS: f64 = 0.008;

/// Why a sensor read did not happen: Python's `Nct679xUnavailable` (no NCT679x
/// responds, or before one has been detected) or the `OSError` a port access
/// or the bus mutex raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Nct679xError {
    Unavailable(String),
    Io(HwError),
}

impl fmt::Display for Nct679xError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Nct679xError::Unavailable(message) => f.write_str(message),
            Nct679xError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for Nct679xError {}

impl From<HwError> for Nct679xError {
    fn from(error: HwError) -> Self {
        Nct679xError::Io(error)
    }
}

/// The named mutex actually taken; unit tests take a private one per thread.
#[cfg(not(test))]
fn bus_mutex_name(name: &str) -> String {
    name.to_string()
}

#[cfg(test)]
fn bus_mutex_name(name: &str) -> String {
    crate::sensors::superio_lpc::bus_mutex_name(name)
}

fn isa_mutex() -> HwResult<NamedMutex> {
    NamedMutex::acquire(&bus_mutex_name(ISA_MUTEX_NAME), DEFAULT_MUTEX_TIMEOUT_MS)
}

fn nct679x_chip_name(chip_id: u16) -> Option<&'static str> {
    NCT679X_CHIP_IDS.iter().find(|(id, _)| *id == chip_id).map(|(_, name)| *name)
}

/// Detect an NCT679x and read its sensor registers. Read-only.
pub struct Nct679xReader {
    lock: Mutex<()>,
    pub config_port: Option<u16>,
    pub chip_id: Option<u16>,
    pub chip_name: Option<&'static str>,
    pub hwm_base: Option<u16>,
    pub last_error: String,
}

impl Default for Nct679xReader {
    fn default() -> Self {
        Self::new()
    }
}

/// What one pass over the configuration ports found.
struct Probe {
    found: Option<(u16, u16, &'static str, u16)>,
    declined: Vec<(u16, u16)>,
}

impl Nct679xReader {
    pub fn new() -> Self {
        Nct679xReader {
            lock: Mutex::new(()),
            config_port: None,
            chip_id: None,
            chip_name: None,
            hwm_base: None,
            last_error: String::new(),
        }
    }

    // -- configuration space (caller holds the mutex) ----------------------

    fn enter_config(port: u16) -> HwResult<()> {
        let io = hw();
        io.outb(port, NUVOTON_UNLOCK_BYTE)?;
        io.outb(port, NUVOTON_UNLOCK_BYTE)
    }

    fn exit_config(port: u16) -> HwResult<()> {
        hw().outb(port, NUVOTON_LOCK_BYTE)
    }

    fn read_config(port: u16, register: u8) -> HwResult<u8> {
        let io = hw();
        io.outb(port, register)?;
        io.inb(port + 1)
    }

    fn select_logical_device(port: u16, ldn: u8) -> HwResult<()> {
        let io = hw();
        io.outb(port, REG_LOGICAL_DEVICE)?;
        io.outb(port + 1, ldn)
    }

    /// The chip ID and window base one port answers with, or `None` when it
    /// does not answer. Config mode is exited whatever happens.
    fn probe_port(port: u16) -> HwResult<Option<(u16, u16)>> {
        let body = (|| -> HwResult<Option<(u16, u16)>> {
            Self::enter_config(port)?;
            let high = Self::read_config(port, REG_CHIP_ID_HIGH)? as u16;
            let low = Self::read_config(port, REG_CHIP_ID_LOW)? as u16;
            let chip_id = (high << 8) | low;
            if chip_id == 0x0000 || chip_id == 0xFFFF {
                return Ok(None);
            }
            Self::select_logical_device(port, LDN_EC_SPACE)?;
            let base_high = Self::read_config(port, REG_BASE_ADDRESS_HIGH)? as u16;
            let base_low = Self::read_config(port, REG_BASE_ADDRESS_LOW)? as u16;
            Ok(Some((chip_id, (base_high << 8) | base_low)))
        })();
        // Never leave the chip unlocked, even on error.
        let exited = Self::exit_config(port);
        let found = body?;
        exited?;
        Ok(found)
    }

    fn probe(&self) -> HwResult<Probe> {
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _bus = isa_mutex()?;
        let mut declined = Vec::new();
        for port in CONFIG_PORTS {
            let (chip_id, base) = match Self::probe_port(port) {
                Ok(Some(found)) => found,
                Ok(None) | Err(_) => continue,
            };
            if base == 0x0000 || base == 0xFFFF {
                continue;
            }
            let Some(name) = nct679x_chip_name(chip_id) else {
                declined.push((port, chip_id));
                continue;
            };
            return Ok(Probe { found: Some((port, chip_id, name, base)), declined });
        }
        Ok(Probe { found: None, declined })
    }

    /// Find an NCT679x and its monitor window. `last_error` explains an
    /// `Ok(false)`; `Err` is the bus mutex not being had.
    pub fn detect(&mut self) -> HwResult<bool> {
        self.last_error = String::new();
        self.hwm_base = None;
        self.chip_id = None;
        self.chip_name = None;
        if !hw().is_driver_open() {
            self.last_error = "InpOut driver is not open".into();
            return Ok(false);
        }
        let probe = self.probe()?;
        if let Some((port, chip_id, name, base)) = probe.found {
            self.config_port = Some(port);
            self.chip_id = Some(chip_id);
            self.chip_name = Some(name);
            self.hwm_base = Some(base);
            return Ok(true);
        }
        if !probe.declined.is_empty() {
            self.last_error = format!(
                "no NCT679x; declined: {}",
                probe
                    .declined
                    .iter()
                    .map(|(port, chip_id)| format!("0x{chip_id:04X} at 0x{port:02X}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        } else {
            self.last_error = "No Super I/O responded on 0x2E or 0x4E".into();
        }
        Ok(false)
    }

    // -- sensor registers ---------------------------------------------------

    /// Select the bank, then the register, then read the data port.
    fn read_locked(base: u16, address: u16) -> HwResult<u8> {
        let io = hw();
        io.outb(base + HWM_INDEX_OFFSET, BANK_SELECT_REGISTER)?;
        io.outb(base + HWM_DATA_OFFSET, ((address >> 8) & 0xFF) as u8)?;
        io.outb(base + HWM_INDEX_OFFSET, (address & 0xFF) as u8)?;
        io.inb(base + HWM_DATA_OFFSET)
    }

    fn base(&self) -> Result<u16, Nct679xError> {
        self.hwm_base
            .ok_or_else(|| Nct679xError::Unavailable("NCT679x has not been detected".into()))
    }

    /// Read one sensor byte addressed as `(bank << 8) | register`.
    pub fn read_byte(&self, address: u16) -> Result<u8, Nct679xError> {
        let base = self.base()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _bus = isa_mutex()?;
        Ok(Self::read_locked(base, address)?)
    }

    /// Read several sensor bytes under one acquisition of the mutex.
    pub fn read_bytes(&self, addresses: &[u16]) -> Result<Vec<u8>, Nct679xError> {
        let base = self.base()?;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _bus = isa_mutex()?;
        addresses
            .iter()
            .map(|&address| Self::read_locked(base, address))
            .collect::<HwResult<Vec<u8>>>()
            .map_err(Nct679xError::Io)
    }
}

/// Decode a temperature byte into degrees Celsius.
///
/// Whole degrees, signed: this chip reports the fractional half-degree in a
/// separate register the displays here do not use, and every reading was
/// confirmed against HWiNFO's own whole-degree presentation of the same chip.
pub fn decode_temperature(raw: u32) -> i32 {
    let raw = (raw & 0xFF) as i32;
    if raw > 127 { raw - 256 } else { raw }
}

/// Decode a voltage byte at the per-channel volts-per-count
/// (`VOLTAGE_STEP_VOLTS` is the starting point).
pub fn decode_volts(raw: u32, step: f64) -> f64 {
    (raw & 0xFF) as f64 * step
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Hardware, set_thread_hardware};
    use crate::sensors::board_sensors::{
        NCT6798D_RAILS, NCT6798D_TEMPERATURES, validate_rail, validate_temperature,
    };
    use std::collections::HashMap;
    use std::sync::Arc;

    struct State {
        writes: Vec<(u16, u8)>,
        answer_on: Option<u16>,
        chip_id: u16,
        sensors: HashMap<u16, u8>,
        config_index: Option<u8>,
        bank: u8,
        index: u8,
        selecting_bank: bool,
        driver_open: bool,
        unlocked: bool,
    }

    /// Answers as the NCT6798D on an ASUS ROG MAXIMUS Z790 APEX.
    struct FakeIo {
        state: Mutex<State>,
    }

    const HWM_BASE: u16 = 0x0290;

    impl FakeIo {
        fn new(sensors: &[(u16, u8)]) -> Self {
            FakeIo {
                state: Mutex::new(State {
                    writes: Vec::new(),
                    answer_on: Some(0x2E),
                    chip_id: 0xD42B,
                    sensors: sensors.iter().copied().collect(),
                    config_index: None,
                    bank: 0,
                    index: 0,
                    selecting_bank: false,
                    driver_open: true,
                    unlocked: false,
                }),
            }
        }

        fn chip(self, chip_id: u16) -> Self {
            self.state.lock().unwrap().chip_id = chip_id;
            self
        }

        fn install(self) -> Arc<FakeIo> {
            let shared = Arc::new(self);
            set_thread_hardware(Some(shared.clone()));
            shared
        }

        fn sent(&self) -> Vec<(u16, u8)> {
            self.state.lock().unwrap().writes.clone()
        }
    }

    impl Hardware for FakeIo {
        fn read_phys(&self, _address: u64, _size: usize) -> HwResult<Vec<u8>> {
            Err(HwError::new("no physical memory here"))
        }
        fn get_phys_long(&self, _address: u64) -> HwResult<u32> {
            Err(HwError::new("no physical memory here"))
        }
        fn set_phys_long(&self, _address: u64, _value: u32) -> HwResult<()> {
            Err(HwError::new("no physical memory here"))
        }
        fn inl(&self, _port: u16) -> HwResult<u32> {
            Err(HwError::new("no dword ports here"))
        }
        fn outl(&self, _port: u16, _value: u32) -> HwResult<()> {
            Err(HwError::new("no dword ports here"))
        }
        fn is_driver_open(&self) -> bool {
            self.state.lock().unwrap().driver_open
        }

        fn outb(&self, port: u16, value: u8) -> HwResult<()> {
            let mut s = self.state.lock().unwrap();
            s.writes.push((port, value));
            if CONFIG_PORTS.contains(&port) {
                if value == NUVOTON_UNLOCK_BYTE {
                    s.unlocked = true;
                } else if value == NUVOTON_LOCK_BYTE {
                    s.unlocked = false;
                } else {
                    s.config_index = Some(value);
                }
                return Ok(());
            }
            if CONFIG_PORTS.contains(&port.wrapping_sub(1)) {
                return Ok(());
            }
            if port == HWM_BASE + HWM_INDEX_OFFSET {
                if value == BANK_SELECT_REGISTER {
                    s.selecting_bank = true;
                } else {
                    s.index = value;
                    s.selecting_bank = false;
                }
                return Ok(());
            }
            if port == HWM_BASE + HWM_DATA_OFFSET && s.selecting_bank {
                s.bank = value;
                s.selecting_bank = false;
            }
            Ok(())
        }

        fn inb(&self, port: u16) -> HwResult<u8> {
            let s = self.state.lock().unwrap();
            let config = port.wrapping_sub(1);
            if CONFIG_PORTS.contains(&config) {
                if Some(config) != s.answer_on {
                    return Ok(0xFF);
                }
                return Ok(match s.config_index {
                    Some(REG_CHIP_ID_HIGH) => (s.chip_id >> 8) as u8,
                    Some(REG_CHIP_ID_LOW) => (s.chip_id & 0xFF) as u8,
                    Some(REG_BASE_ADDRESS_HIGH) => (HWM_BASE >> 8) as u8,
                    Some(REG_BASE_ADDRESS_LOW) => (HWM_BASE & 0xFF) as u8,
                    _ => 0x00,
                });
            }
            if port == HWM_BASE + HWM_DATA_OFFSET {
                let address = ((s.bank as u16) << 8) | s.index as u16;
                return Ok(*s.sensors.get(&address).unwrap_or(&0x00));
            }
            Ok(0x00)
        }
    }

    fn make_reader(io: FakeIo) -> (Nct679xReader, Arc<FakeIo>) {
        (Nct679xReader::new(), io.install())
    }

    fn assert_close(actual: f64, expected: f64, places: i32) {
        assert!(
            (actual - expected).abs() < 0.5 * 10f64.powi(-places),
            "{actual} != {expected} to {places} places"
        );
    }

    // -- DetectionTest -----------------------------------------------------

    #[test]
    fn detects_the_chip_and_its_monitor_window() {
        let (mut reader, _) = make_reader(FakeIo::new(&[]));
        assert!(reader.detect().unwrap());
        assert_eq!(reader.chip_id, Some(0xD42B));
        assert_eq!(reader.chip_name, Some("NCT6798D"));
        assert_eq!(reader.hwm_base, Some(0x0290));
        assert_eq!(reader.config_port, Some(0x2E));
    }

    #[test]
    fn an_unknown_chip_is_declined_and_leaves_no_window() {
        // NCT6687D: a real chip, but not this family.
        let (mut reader, _) = make_reader(FakeIo::new(&[]).chip(0xD592));
        assert!(!reader.detect().unwrap());
        assert_eq!(reader.hwm_base, None);
        assert!(reader.last_error.contains("0xD592"));
    }

    #[test]
    fn detection_relocks_the_config_window() {
        let (mut reader, io) = make_reader(FakeIo::new(&[]));
        reader.detect().unwrap();
        assert!(!io.state.lock().unwrap().unlocked);
    }

    #[test]
    fn declining_also_relocks_the_config_window() {
        let (mut reader, io) = make_reader(FakeIo::new(&[]).chip(0xD592));
        reader.detect().unwrap();
        assert!(!io.state.lock().unwrap().unlocked);
    }

    #[test]
    fn reading_before_detection_is_refused() {
        let (reader, _) = make_reader(FakeIo::new(&[]));
        assert!(matches!(reader.read_byte(0x491), Err(Nct679xError::Unavailable(_))));
    }

    #[test]
    fn a_closed_driver_is_reported_not_raised() {
        let io = FakeIo::new(&[]);
        io.state.lock().unwrap().driver_open = false;
        let (mut reader, _) = make_reader(io);
        assert!(!reader.detect().unwrap());
        assert!(reader.last_error.contains("driver"));
    }

    // -- RegisterAddressingTest --------------------------------------------

    #[test]
    fn a_sensor_byte_is_addressed_bank_then_register() {
        let (mut reader, io) = make_reader(FakeIo::new(&[(0x491, 0x21)]));
        assert!(reader.detect().unwrap());
        io.state.lock().unwrap().writes.clear();
        assert_eq!(reader.read_byte(0x491).unwrap(), 0x21);
        // Bank select on the index port, bank on the data port, then register.
        assert_eq!(
            io.sent(),
            vec![
                (0x0290 + HWM_INDEX_OFFSET, BANK_SELECT_REGISTER),
                (0x0290 + HWM_DATA_OFFSET, 0x04),
                (0x0290 + HWM_INDEX_OFFSET, 0x91),
            ]
        );
    }

    #[test]
    fn bank_zero_registers_read_from_bank_zero() {
        let (mut reader, _) = make_reader(FakeIo::new(&[(0x027, 0x1F)]));
        reader.detect().unwrap();
        assert_eq!(reader.read_byte(0x027).unwrap(), 0x1F);
    }

    #[test]
    fn several_bytes_read_under_one_lock() {
        let (mut reader, _) = make_reader(FakeIo::new(&[(0x491, 0x21), (0x401, 0x31)]));
        reader.detect().unwrap();
        assert_eq!(reader.read_bytes(&[0x491, 0x401]).unwrap(), vec![0x21, 0x31]);
    }

    // -- DecodeTest --------------------------------------------------------

    #[test]
    fn whole_degrees() {
        assert_eq!(decode_temperature(0x21), 33);
        assert_eq!(decode_temperature(0x31), 49);
        assert_eq!(decode_temperature(0x1F), 31);
    }

    #[test]
    fn a_negative_reading_is_signed() {
        assert_eq!(decode_temperature(0xFF), -1);
        assert_eq!(decode_temperature(0x80), -128);
    }

    #[test]
    fn volts_at_the_per_channel_step() {
        assert_close(decode_volts(0x92, 0.009), 1.314, 3);
        assert_close(decode_volts(0x4A, 0.016), 1.184, 3);
        assert_close(decode_volts(0x73, 0.016), 1.840, 3);
        assert_close(decode_volts(0x4C, 0.018), 1.368, 3);
    }

    #[test]
    fn the_block_is_not_uniformly_eight_millivolts() {
        // The trap this map fell into once: the droop identified the core
        // channel, and the block's usual step was assumed to come with it.
        // At 8 mV the same raw reads 1.168 V against HWiNFO's 1.314.
        assert_close(decode_volts(0x92, 0.008), 1.168, 3);
        let mut steps: Vec<f64> = NCT6798D_RAILS.iter().map(|(_, (_, step))| *step).collect();
        steps.sort_by(f64::total_cmp);
        steps.dedup();
        assert!(steps.len() > 1);
    }

    #[test]
    fn the_memory_controller_step_is_two_counts_across_hwinfo_range() {
        // HWiNFO watched IMC VDD move 1.332-1.368 V over the capture window.
        // At 18 mV that is raw 74 to 76; no other step puts both endpoints on
        // whole counts two apart, which is what fixes the step independently
        // of the single-point match.
        assert_close(decode_volts(74, 0.018), 1.332, 3);
        assert_close(decode_volts(76, 0.018), 1.368, 3);
    }

    // -- ConfirmedCaptureTest: the map must keep decoding to the captured
    // HWiNFO values.

    const CAPTURE: &[(u16, u32)] = &[
        (0x491, 0x21), // CPU        33 C, rises to 41-42 C loaded
        (0x073, 0x1E), // CPU Package 30 C
        (0x401, 0x31), // PCH        49 C, flat under load
        (0x027, 0x1F), // System     31 C, flat under load
        (0x480, 0x92), // Vcore      1.314 V at 9 mV, droops under load
        (0x481, 0x7E), // +5V        5.040 V
        (0x482, 0xD7), // AVSB        3.440 V
        (0x483, 0xD0), // 3VCC        3.328 V
        (0x484, 0x7D), // +12V       12.000 V
        (0x485, 0x8B), // VIN8        1.112 V
        (0x486, 0x93), // VIN4        1.176 V
        (0x487, 0xD7), // 3VSB_ATX    3.440 V
        (0x488, 0xCA), // BAT_3V      3.232 V
        (0x489, 0x41), // VTT        1.040 V (HWiNFO's minimum for the channel)
        (0x48A, 0x53), // DRAM       1.494 V on the Z790-A D4
        (0x48B, 0x46), // CPU L2      1.120 V
        (0x48C, 0x84), // VIN2        1.056 V
        (0x48D, 0x54), // CPU VCCSA   1.344 V
        (0x48E, 0x73), // CPU AUX    1.840 V
        (0x48F, 0x79), // VIN9        0.968 V
        (0x470, 0x71), // VHIF        1.808 V
        (0x0FB, 0x24), // not mapped: reads 36 but never moves. See below.
    ];

    fn captured(address: u16) -> u32 {
        CAPTURE.iter().find(|(a, _)| *a == address).unwrap().1
    }

    #[test]
    fn temperatures_decode_to_the_hwinfo_readings() {
        let expected = [
            ("motherboard", 31),
            ("cpu_weighted", 31),
            ("cpu_package", 30),
            ("cpu", 33),
            ("pch", 49),
            ("system", 31),
        ];
        for (key, address) in NCT6798D_TEMPERATURES {
            let want = expected.iter().find(|(k, _)| k == key).unwrap().1;
            assert_eq!(decode_temperature(captured(*address)), want, "{key}");
        }
    }

    #[test]
    fn rails_decode_to_the_hwinfo_readings() {
        let expected = [
            ("vcore", 1.314), ("plus5v", 5.040), ("avsb", 3.440),
            ("v3cc", 3.328), ("plus12v", 12.000), ("vin8", 1.112),
            ("vin4", 1.176), ("v3sb_atx", 3.440), ("bat_3v", 3.232),
            ("vtt", 1.040), ("vdd2", 1.494), ("vdimm", 1.494),
            ("cpu_l2", 1.120), ("vin2", 1.056), ("cpu_sa", 1.344),
            ("cpu_aux", 1.840), ("vin9", 0.968), ("vhif", 1.808),
        ];
        for (key, (address, step)) in NCT6798D_RAILS {
            let want = expected.iter().find(|(k, _)| k == key).unwrap().1;
            assert_close(decode_volts(captured(*address), *step), want, 3);
        }
    }

    #[test]
    fn every_mapped_reading_lands_inside_its_band() {
        for (key, address) in NCT6798D_TEMPERATURES {
            let celsius = decode_temperature(captured(*address)) as f64;
            assert!(validate_temperature(key, celsius).is_some(), "temperature {key}");
        }
        for (key, (address, step)) in NCT6798D_RAILS {
            let volts = decode_volts(captured(*address), *step);
            assert!(validate_rail(key, volts).is_some(), "rail {key}");
        }
    }

    #[test]
    fn the_static_lookalike_is_not_mapped() {
        // 0x0FB reads exactly 36, the sole match anywhere in the register space
        // for HWiNFO's "CPU Package 36 C", and it is still not a CPU sensor: it
        // did not move a degree across a full load cycle. Claiming it would be
        // the resemblance error this project keeps refusing to make.
        assert!(!NCT6798D_TEMPERATURES.iter().any(|(_, address)| *address == 0x0FB));
    }

    #[test]
    fn sensors_this_board_does_not_carry_stay_unmapped() {
        // HWiNFO lists no VRM or socket sensor under this chip; its VR VCC
        // temperature is SVID telemetry on another transport. A blank row beats
        // a borrowed one.
        assert!(!NCT6798D_TEMPERATURES.iter().any(|(key, _)| *key == "vrm"));
        assert!(!NCT6798D_TEMPERATURES.iter().any(|(key, _)| *key == "socket"));
        // Input 10 changes meaning with the ASUS board layout.  DDR5 exposes
        // it as VDD2 and reads DRAM from the module PMIC; DDR4 exposes the
        // same electrical channel as DRAM.  Platform filtering chooses one.
        let rail = |name: &str| NCT6798D_RAILS.iter().find(|(key, _)| *key == name).unwrap().1;
        assert_eq!(rail("vdimm"), rail("vdd2"));
    }

    // -- SafetyTest --------------------------------------------------------

    #[test]
    fn module_defines_no_write_primitive() {
        let source = include_str!("nct679x.rs");
        // Selection writes go through read_locked and the config helpers only.
        assert!(!source.contains(concat!("fn ", "write")));
    }
}
