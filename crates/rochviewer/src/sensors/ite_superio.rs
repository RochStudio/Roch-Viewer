//! ITE Super I/O sensor transport for Intel boards.
//!
//! `superio_lpc` speaks the Nuvoton unlock sequence and only accepts NCT668x
//! parts. Gigabyte's Z890 boards carry ITE silicon instead -- IT8696E on config
//! port 0x2E and IT87952E on 0x4E -- which answers a different unlock and reads
//! its sensors through a plain index/data pair rather than a 16-bit window. A
//! Nuvoton-only reader finds nothing there and every board rail reads blank.
//!
//! This module is deliberately standalone. It does not import `superio_lpc`,
//! because that module pulls in `lowlevel_io`, and the Intel path must not import
//! AMD code to read a board rail. (In the Rust port the named mutex is the
//! shared `lowlevel_io::NamedMutex`, which is no AMD code; everything else
//! stays this module's own.)
//!
//! READ-ONLY. Only three things are ever written: the vendor unlock sequence,
//! the logical-device selector, and the EC index register. All three select what
//! to read. No sensor, fan or PWM value is written.

use std::collections::BTreeMap;
use std::ops::Range;
use std::sync::Mutex;

use crate::hardware::lowlevel_io::NamedMutex;
use crate::hardware::{HwResult, hw};

/// The mutex mainstream monitoring tools take before driving LPC, so a
/// concurrent HWiNFO poll cannot land between our index write and data read.
pub const ISA_MUTEX_NAME: &str = "Global\\Access_ISABUS.HTP.Method";

pub const CONFIG_PORTS: [u16; 2] = [0x2E, 0x4E];

/// ITE MB PnP unlock: 0x87, 0x01, 0x55, then 0x55 on port 0x2E or 0xAA on 0x4E.
pub const ITE_UNLOCK: [u8; 3] = [0x87, 0x01, 0x55];
pub const ITE_EXIT_REGISTER: u8 = 0x02;
pub const ITE_EXIT_VALUE: u8 = 0x02;

pub const REG_CHIP_ID_HIGH: u8 = 0x20;
pub const REG_CHIP_ID_LOW: u8 = 0x21;
pub const REG_LOGICAL_DEVICE: u8 = 0x07;
pub const REG_BASE_ADDRESS_HIGH: u8 = 0x60;
pub const REG_BASE_ADDRESS_LOW: u8 = 0x61;

/// Logical device 4 is the environment controller on every ITE part here.
pub const LDN_ENVIRONMENT_CONTROLLER: u8 = 0x04;

// Sensor access within the EC window: write the register to base+5, read the
// value from base+6.
pub const EC_INDEX_OFFSET: u16 = 5;
pub const EC_DATA_OFFSET: u16 = 6;

/// Confirmed present on Gigabyte Z890 AORUS TACHYON ICE.
pub const CHIP_IDS: &[(u16, &str)] = &[(0x8696, "IT8696E"), (0x8695, "IT87952E")];

/// Voltage LSB per chip. These genuinely differ: decoding IT8696E at 10.9 mV
/// or IT87952E at 12 mV misses every rail by 8-10%, and both figures were
/// settled by matching HWiNFO on the same boot rather than assumed.
pub const CHIP_VOLTAGE_STEP: &[(&str, f64)] = &[("IT8696E", 0.012), ("IT87952E", 0.0109)];

pub const VOLTAGE_BLOCK: Range<u8> = 0x20..0x29;
pub const TEMPERATURE_BLOCK: Range<u8> = 0x29..0x2E;

// A temperature channel with nothing attached reads back at the rail ends.
pub const TEMPERATURE_MIN_C: i32 = 1;
pub const TEMPERATURE_MAX_C: i32 = 125;

pub const MUTEX_TIMEOUT_MS: u32 = 500;

/// The named mutex actually taken; unit tests take a private one per thread.
#[cfg(not(test))]
fn bus_mutex_name(name: &str) -> String {
    name.to_string()
}

#[cfg(test)]
pub(crate) fn bus_mutex_name(name: &str) -> String {
    format!(
        "Local\\RochViewerTest-{}-{:?}-{}",
        std::process::id(),
        std::thread::current().id(),
        name.replace('\\', "_")
    )
}

/// Windows named mutex guarding the LPC bus.
///
/// Acquisition is not optional. HWiNFO and every other monitoring tool take
/// this same mutex before driving Super I/O, because the chip is addressed
/// through one index/data pair: if a second writer sets the index between
/// another reader's index write and its data read, that reader gets a value
/// from the wrong register -- and interleaved writes can leave the embedded
/// controller wedged, which takes the machine down rather than the app.
///
/// So a failed wait means "do not touch the hardware", never "carry on":
/// `None` here is a decline, and no mutex at all (it could not be created)
/// is the same decline, since there is then no way to exclude another
/// writer. What was taken is released on drop, and only that.
fn bus_mutex() -> Option<NamedMutex> {
    NamedMutex::acquire(&bus_mutex_name(ISA_MUTEX_NAME), MUTEX_TIMEOUT_MS).ok()
}

/// One ITE part found on a configuration port.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IteChip {
    pub port: u16,
    pub chip_id: u16,
    pub ec_base: u16,
    pub step: Option<f64>,
}

/// Detect ITE Super I/O chips and read their sensor block. Read-only.
pub struct IteSuperIoReader {
    lock: Mutex<()>,
    pub chips: BTreeMap<&'static str, IteChip>,
    /// The identity rows ask every sensor reader for chip_name, whatever
    /// transport it speaks. This board answers on both config ports, so the
    /// attribute names each chip found rather than only the first.
    pub chip_name: Option<String>,
    last_error: Mutex<String>,
}

impl Default for IteSuperIoReader {
    fn default() -> Self {
        Self::new()
    }
}

impl IteSuperIoReader {
    pub fn new() -> Self {
        IteSuperIoReader {
            lock: Mutex::new(()),
            chips: BTreeMap::new(),
            chip_name: None,
            last_error: Mutex::new(String::new()),
        }
    }

    /// Why the last detection or read came back empty, or "".
    pub fn last_error(&self) -> String {
        self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_last_error(&self, message: &str) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = message.to_string();
    }

    // -- raw port access ---------------------------------------------------

    fn outb(port: u16, value: u8) -> HwResult<()> {
        hw().outb(port, value)
    }

    fn inb(port: u16) -> HwResult<u8> {
        hw().inb(port)
    }

    fn enter_config(port: u16) -> HwResult<()> {
        for value in ITE_UNLOCK {
            Self::outb(port, value)?;
        }
        Self::outb(port, if port == 0x2E { 0x55 } else { 0xAA })
    }

    fn exit_config(port: u16) -> HwResult<()> {
        Self::outb(port, ITE_EXIT_REGISTER)?;
        Self::outb(port + 1, ITE_EXIT_VALUE)
    }

    fn read_config(port: u16, register: u8) -> HwResult<u8> {
        Self::outb(port, register)?;
        Self::inb(port + 1)
    }

    /// One port's chip, or `None` when it does not answer as a known part.
    /// Config mode is exited whatever happens.
    fn probe_port(port: u16) -> HwResult<Option<(&'static str, u16, u16)>> {
        let body = (|| -> HwResult<Option<(&'static str, u16, u16)>> {
            Self::enter_config(port)?;
            let chip_id = ((Self::read_config(port, REG_CHIP_ID_HIGH)? as u16) << 8)
                | Self::read_config(port, REG_CHIP_ID_LOW)? as u16;
            if chip_id == 0x0000 || chip_id == 0xFFFF {
                return Ok(None);
            }
            let Some(name) = CHIP_IDS.iter().find(|(id, _)| *id == chip_id).map(|(_, n)| *n)
            else {
                // Unknown ITE part: the register layout is close
                // enough to read, but the rail map is not ours to
                // guess, so record it without claiming support.
                return Ok(None);
            };
            Self::outb(port, REG_LOGICAL_DEVICE)?;
            Self::outb(port + 1, LDN_ENVIRONMENT_CONTROLLER)?;
            let base = ((Self::read_config(port, REG_BASE_ADDRESS_HIGH)? as u16) << 8)
                | Self::read_config(port, REG_BASE_ADDRESS_LOW)? as u16;
            Ok(Some((name, chip_id, base)))
        })();
        // Never leave the chip unlocked, even on error.
        let exited = Self::exit_config(port);
        let found = body?;
        exited?;
        Ok(found)
    }

    // -- detection ---------------------------------------------------------

    /// Populate `chips` with every ITE part found. True if any answered.
    pub fn detect(&mut self) -> bool {
        self.chips = BTreeMap::new();
        self.set_last_error("");
        let mut found: BTreeMap<&'static str, IteChip> = BTreeMap::new();
        {
            let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
            let Some(_bus) = bus_mutex() else {
                self.set_last_error("LPC bus is held by another tool; declined to probe");
                return false;
            };
            for port in CONFIG_PORTS {
                let (name, chip_id, base) = match Self::probe_port(port) {
                    Ok(Some(chip)) => chip,
                    Ok(None) | Err(_) => continue,
                };
                if base == 0x0000 || base == 0xFFFF {
                    continue;
                }
                let step = CHIP_VOLTAGE_STEP.iter().find(|(n, _)| *n == name).map(|(_, s)| *s);
                found.insert(name, IteChip { port, chip_id, ec_base: base, step });
            }
        }
        self.chips = found;
        // Both parts, in config-port order, because the pair is what this
        // board has -- naming one of them would read as the whole answer.
        let mut ordered: Vec<(&'static str, &IteChip)> =
            self.chips.iter().map(|(name, chip)| (*name, chip)).collect();
        ordered.sort_by_key(|(_, chip)| chip.port);
        let joined = ordered.iter().map(|(name, _)| *name).collect::<Vec<_>>().join(" + ");
        self.chip_name = if joined.is_empty() { None } else { Some(joined) };
        if self.chips.is_empty() {
            self.set_last_error("No ITE Super I/O responded on 0x2E or 0x4E");
        }
        !self.chips.is_empty()
    }

    // -- sensor window -----------------------------------------------------

    /// Read one EC sensor register, or `None` when the chip is absent.
    pub fn read_register(&self, chip_name: &str, register: u8) -> Option<u8> {
        let base = self.chips.get(chip_name)?.ec_base;
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(_bus) = bus_mutex() else {
            // Another tool owns the bus. Reporting nothing is correct;
            // reading anyway would race its index write.
            self.set_last_error("LPC bus busy; sensor read skipped");
            return None;
        };
        Self::outb(base + EC_INDEX_OFFSET, register).ok()?;
        Self::inb(base + EC_DATA_OFFSET).ok()
    }

    /// Read several EC registers under one hold of the bus.
    ///
    /// Returns `{register: value}`, with `None` for any that failed, or {}
    /// when the chip is absent or another tool holds the bus. One mutex
    /// acquisition for the lot: a board's whole sensor block per tick would
    /// otherwise be a mutex round trip per register, each able to wait out
    /// MUTEX_TIMEOUT_MS, and a fan's two tachometer bytes would be read
    /// under different holds.
    pub fn read_registers(&self, chip_name: &str, registers: &[u8]) -> BTreeMap<u8, Option<u8>> {
        let Some(chip) = self.chips.get(chip_name) else {
            return BTreeMap::new();
        };
        let base = chip.ec_base;
        let mut values = BTreeMap::new();
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let Some(_bus) = bus_mutex() else {
            self.set_last_error("LPC bus busy; sensor read skipped");
            return BTreeMap::new();
        };
        for &register in registers {
            let value = Self::outb(base + EC_INDEX_OFFSET, register)
                .and_then(|()| Self::inb(base + EC_DATA_OFFSET))
                .ok();
            values.insert(register, value);
        }
        values
    }

    /// The chip's volts per count, or `None` when it is absent.
    pub fn voltage_step(&self, chip_name: &str) -> Option<f64> {
        self.chips.get(chip_name).and_then(|chip| chip.step)
    }

    /// Return a decoded rail in volts, or `None` (`divider` is 1.0 for an
    /// undivided input).
    pub fn read_voltage(&self, chip_name: &str, register: u8, divider: f64) -> Option<f64> {
        let step = self.chips.get(chip_name)?.step?;
        if step == 0.0 {
            return None;
        }
        let raw = self.read_register(chip_name, register)?;
        Some(raw as f64 * step * divider)
    }

    /// Return a plausible temperature in Celsius, or `None`.
    pub fn read_temperature(&self, chip_name: &str, register: u8) -> Option<f64> {
        let raw = self.read_register(chip_name, register)? as i32;
        if !(TEMPERATURE_MIN_C..=TEMPERATURE_MAX_C).contains(&raw) {
            // An unconnected channel parks outside the band; 0xC9 shows up on
            // this board's second chip for a header with no probe fitted.
            return None;
        }
        Some(raw as f64)
    }

    /// Return the whole sensor block, for confirming a new board.
    pub fn read_block(&self, chip_name: &str) -> BTreeMap<u8, Option<u8>> {
        if !self.chips.contains_key(chip_name) {
            return BTreeMap::new();
        }
        let mut block = BTreeMap::new();
        for register in VOLTAGE_BLOCK.chain(TEMPERATURE_BLOCK) {
            block.insert(register, self.read_register(chip_name, register));
        }
        block
    }
}

/// A stand-in for the ITE pair, shared with the modules that build on it.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::hardware::{Hardware, HwError, set_thread_hardware};
    use std::collections::HashMap;
    use std::sync::Arc;

    pub struct State {
        pub writes: Vec<(u16, u8)>,
        /// port -> (chip id, EC base)
        pub chips: HashMap<u16, (u16, u16)>,
        /// EC base -> register -> value
        pub registers: HashMap<u16, HashMap<u8, u8>>,
        /// EC bases whose data port fails to read.
        pub failing: Vec<u16>,
        config_index: HashMap<u16, u8>,
        ec_index: HashMap<u16, u8>,
        unlock_progress: HashMap<u16, usize>,
        pub unlocked: HashMap<u16, bool>,
    }

    /// ITE parts answering the MB PnP unlock, with an EC sensor window each.
    pub struct FakeIte {
        pub state: Mutex<State>,
    }

    impl FakeIte {
        pub fn new() -> Self {
            FakeIte {
                state: Mutex::new(State {
                    writes: Vec::new(),
                    chips: HashMap::new(),
                    registers: HashMap::new(),
                    failing: Vec::new(),
                    config_index: HashMap::new(),
                    ec_index: HashMap::new(),
                    unlock_progress: HashMap::new(),
                    unlocked: HashMap::new(),
                }),
            }
        }

        pub fn chip(self, port: u16, chip_id: u16, ec_base: u16, registers: &[(u8, u8)]) -> Self {
            {
                let mut s = self.state.lock().unwrap();
                s.chips.insert(port, (chip_id, ec_base));
                s.registers.insert(ec_base, registers.iter().copied().collect());
            }
            self
        }

        /// The IT8696E at 0x2E, window 0x0A40, as on the X870/Z890 TACHYONs.
        pub fn it8696e(registers: &[(u8, u8)]) -> Self {
            FakeIte::new().chip(0x2E, 0x8696, 0x0A40, registers)
        }

        pub fn install(self) -> Arc<FakeIte> {
            let shared = Arc::new(self);
            set_thread_hardware(Some(shared.clone()));
            shared
        }

        /// Registers selected on an EC index port, in order.
        pub fn index_writes(&self, ec_base: u16) -> Vec<u8> {
            let s = self.state.lock().unwrap();
            s.writes
                .iter()
                .filter(|(port, _)| *port == ec_base + EC_INDEX_OFFSET)
                .map(|(_, value)| *value)
                .collect()
        }

    }

    impl Hardware for FakeIte {
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
            true
        }

        fn outb(&self, port: u16, value: u8) -> HwResult<()> {
            let mut s = self.state.lock().unwrap();
            s.writes.push((port, value));
            if CONFIG_PORTS.contains(&port) {
                let key = if port == 0x2E { 0x55 } else { 0xAA };
                let progress = *s.unlock_progress.get(&port).unwrap_or(&0);
                let expected = [ITE_UNLOCK[0], ITE_UNLOCK[1], ITE_UNLOCK[2], key];
                if !s.unlocked.get(&port).copied().unwrap_or(false) {
                    if value == expected[progress] {
                        if progress == 3 {
                            s.unlocked.insert(port, true);
                            s.unlock_progress.insert(port, 0);
                        } else {
                            s.unlock_progress.insert(port, progress + 1);
                        }
                    } else {
                        s.unlock_progress.insert(port, 0);
                    }
                    return Ok(());
                }
                s.config_index.insert(port, value);
                return Ok(());
            }
            let config = port.wrapping_sub(1);
            if CONFIG_PORTS.contains(&config) {
                if s.config_index.get(&config) == Some(&ITE_EXIT_REGISTER)
                    && value == ITE_EXIT_VALUE
                {
                    s.unlocked.insert(config, false);
                }
                return Ok(());
            }
            let ec_base = port.wrapping_sub(EC_INDEX_OFFSET);
            if s.registers.contains_key(&ec_base) {
                s.ec_index.insert(ec_base, value);
            }
            Ok(())
        }

        fn inb(&self, port: u16) -> HwResult<u8> {
            let s = self.state.lock().unwrap();
            let config = port.wrapping_sub(1);
            if CONFIG_PORTS.contains(&config) {
                let Some(&(chip_id, ec_base)) = s.chips.get(&config) else {
                    return Ok(0xFF);
                };
                if !s.unlocked.get(&config).copied().unwrap_or(false) {
                    return Ok(0xFF);
                }
                return Ok(match s.config_index.get(&config) {
                    Some(&REG_CHIP_ID_HIGH) => (chip_id >> 8) as u8,
                    Some(&REG_CHIP_ID_LOW) => (chip_id & 0xFF) as u8,
                    Some(&REG_BASE_ADDRESS_HIGH) => (ec_base >> 8) as u8,
                    Some(&REG_BASE_ADDRESS_LOW) => (ec_base & 0xFF) as u8,
                    _ => 0x00,
                });
            }
            let ec_base = port.wrapping_sub(EC_DATA_OFFSET);
            if let Some(registers) = s.registers.get(&ec_base) {
                if s.failing.contains(&ec_base) {
                    return Err(HwError::new("EC data port did not answer"));
                }
                let index = s.ec_index.get(&ec_base).copied().unwrap_or(0);
                return Ok(*registers.get(&index).unwrap_or(&0xFF));
            }
            Ok(0xFF)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::FakeIte;
    use super::*;

    // No Python test module covers this transport on its own; these pin the
    // behaviour am5_board_rails and board_sensors depend on.

    #[test]
    fn detects_both_parts_and_names_them_in_port_order() {
        FakeIte::new()
            .chip(0x4E, 0x8695, 0x0B10, &[])
            .chip(0x2E, 0x8696, 0x0A40, &[])
            .install();
        let mut reader = IteSuperIoReader::new();
        assert!(reader.detect());
        assert_eq!(reader.chip_name.as_deref(), Some("IT8696E + IT87952E"));
        assert_eq!(reader.voltage_step("IT8696E"), Some(0.012));
        assert_eq!(reader.voltage_step("IT87952E"), Some(0.0109));
        assert_eq!(reader.chips["IT87952E"].ec_base, 0x0B10);
    }

    #[test]
    fn config_mode_is_exited_after_detection() {
        let io = FakeIte::it8696e(&[]).install();
        let mut reader = IteSuperIoReader::new();
        assert!(reader.detect());
        assert!(!io.state.lock().unwrap().unlocked.values().any(|u| *u));
    }

    #[test]
    fn nothing_answering_is_reported() {
        FakeIte::new().install();
        let mut reader = IteSuperIoReader::new();
        assert!(!reader.detect());
        assert_eq!(reader.chip_name, None);
        assert_eq!(reader.last_error(), "No ITE Super I/O responded on 0x2E or 0x4E");
    }

    #[test]
    fn temperatures_outside_the_band_are_not_readings() {
        FakeIte::it8696e(&[(0x2A, 37), (0x2B, 0xC9), (0x2C, 0)]).install();
        let mut reader = IteSuperIoReader::new();
        assert!(reader.detect());
        assert_eq!(reader.read_temperature("IT8696E", 0x2A), Some(37.0));
        assert_eq!(reader.read_temperature("IT8696E", 0x2B), None);
        assert_eq!(reader.read_temperature("IT8696E", 0x2C), None);
        assert_eq!(reader.read_temperature("IT87952E", 0x2A), None);
    }

    #[test]
    fn voltages_use_the_chip_step_and_divider() {
        FakeIte::it8696e(&[(0x26, 109), (0x22, 168)]).install();
        let mut reader = IteSuperIoReader::new();
        assert!(reader.detect());
        assert!((reader.read_voltage("IT8696E", 0x26, 1.0).unwrap() - 1.308).abs() < 1e-9);
        assert!((reader.read_voltage("IT8696E", 0x22, 6.0).unwrap() - 12.096).abs() < 1e-9);
    }

    #[test]
    fn a_busy_bus_reads_nothing() {
        FakeIte::it8696e(&[(0x26, 109)]).install();
        let mut reader = IteSuperIoReader::new();
        assert!(reader.detect());
        let name = bus_mutex_name(ISA_MUTEX_NAME);
        let (held, release) = (std::sync::mpsc::channel(), std::sync::mpsc::channel::<()>());
        let holder = std::thread::spawn(move || {
            let _bus = NamedMutex::acquire(&name, 1000).unwrap();
            held.0.send(()).unwrap();
            release.1.recv().ok();
        });
        held.1.recv().unwrap();
        assert!(reader.read_registers("IT8696E", &[0x26]).is_empty());
        assert_eq!(reader.read_register("IT8696E", 0x26), None);
        assert_eq!(reader.last_error(), "LPC bus busy; sensor read skipped");
        release.0.send(()).unwrap();
        holder.join().unwrap();
        assert_eq!(reader.read_register("IT8696E", 0x26), Some(109));
    }
}
