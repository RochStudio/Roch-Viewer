//! Read-only Super I/O (LPC) sensor access.
//!
//! Third and last transport in this project.  Some rails live nowhere else:
//!
//! ```text
//!   * SMU PM table  -> VDDCR_SOC, VDDG, CLDO, CDD_MISC   (amd_smu_voltages)
//!   * DDR5 PMIC     -> DRAM VDD/VDDQ/VPP                 (ddr5_pmic)
//!   * Super I/O     -> CPU VDDIO, Vcore, VTT             (this module)
//! ```
//!
//! CPU VDDIO is provably absent from the first two: raising it 1.44 -> 1.47 V in
//! BIOS moved nothing in the PM table, and the DDR5 PMIC exposes only SWA/SWB/SWC,
//! VIN_Bulk and its two VOUT rails.  HWiNFO reports VDDIO, Vcore and VTT in its
//! motherboard section, which is this chip.
//!
//! SAFETY — this module reads sensors and nothing else:
//!
//!   * The only writes are the ones the read path requires: the vendor unlock
//!     sequence, the logical-device selector used to find the sensor window, and
//!     the page/index registers that address a sensor byte.  Every one of those
//!     selects *what to read*.
//!   * There is no write to any sensor, fan, PWM or configuration value, and no
//!     primitive that could perform one.  The Super I/O also drives fan control,
//!     so that capability is deliberately absent rather than merely unused.
//!   * Config mode is always exited, including on error, mirroring the SMN
//!     selector-restore discipline used elsewhere in this project.
//!   * Access is serialised on the named mutex LibreHardwareMonitor and HWiNFO
//!     take for ISA/LPC access.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};

use crate::hardware::lowlevel_io::{DEFAULT_MUTEX_TIMEOUT_MS, NamedMutex};
use crate::hardware::{HwError, HwResult, hw};
use crate::memo::Memo;
// Believable ranges live with the rail definitions, so the transport and the
// view can never disagree about what counts as a plausible reading.
use crate::sensors::voltage_rails::{rail_by_key, validate_voltage};

/// The mutex mainstream monitoring tools take before driving LPC.
pub const ISA_MUTEX_NAME: &str = "Global\\Access_ISABUS.HTP.Method";

/// Super I/O configuration windows. Boards use one or the other.
pub const CONFIG_PORTS: [u16; 2] = [0x2E, 0x4E];

// Nuvoton unlock/lock sequence.
pub const NUVOTON_UNLOCK_BYTE: u8 = 0x87;
pub const NUVOTON_LOCK_BYTE: u8 = 0xAA;

// Configuration registers.
pub const REG_LOGICAL_DEVICE: u8 = 0x07;
pub const REG_CHIP_ID_HIGH: u8 = 0x20;
pub const REG_CHIP_ID_LOW: u8 = 0x21;
pub const REG_BASE_ADDRESS_HIGH: u8 = 0x60;
pub const REG_BASE_ADDRESS_LOW: u8 = 0x61;

/// Logical device carrying the embedded-controller sensor window.
pub const LDN_EC_SPACE: u8 = 0x0B;

// Offsets inside the EC window.
pub const EC_PAGE_OFFSET: u16 = 0x04;
pub const EC_INDEX_OFFSET: u16 = 0x05;
pub const EC_DATA_OFFSET: u16 = 0x06;

/// The page register needs this preamble before the bank number; without it
/// every read returns the same byte.
pub const EC_PAGE_SELECT: u8 = 0xFF;

/// Known chip IDs. The high byte identifies the family; the low nibble is a
/// revision, so compare on the masked value.
pub const CHIP_IDS: &[(u16, &str)] = &[
    (0xD592, "NCT6687D"),
    (0xD591, "NCT6687D"),
    (0xC732, "NCT6683D"),
    (0xD452, "NCT6686D"),
    (0xC562, "NCT6791D"),
    (0xC803, "NCT6796D"),
    (0xD121, "NCT6798D"),
    // Named by HWiNFO reading this same chip on an ASUS ROG MAXIMUS Z790 APEX.
    // Listed so the refusal below can name what it declined; the sensors are
    // read by nct679x, not from here.
    (0xD42B, "NCT6798D"),
];

/// The name `CHIP_IDS` gives an ID, if any.
pub fn chip_id_name(chip_id: u16) -> Option<&'static str> {
    CHIP_IDS.iter().find(|(id, _)| *id == chip_id).map(|(_, name)| *name)
}

/// Chips whose sensors the page/index/data window below actually addresses.
///
/// Being named in CHIP_IDS is not the same as being readable here. The window
/// this reader drives -- EC page/index/data at ec_base+4/5/6, sensors in the
/// 0x100 and 0x120 blocks -- is the NCT668x embedded-controller model. The
/// NCT679x parts expose their sensors through a different, bank-switched
/// register model, and they do not fault when addressed this way: they answer
/// 0xFFFF and repeated bytes.
///
/// That silence is why this gate has to exist rather than being left to the
/// per-reading band checks. A voltage band rejects 0xFFFF, so the rails simply
/// went blank. A temperature band does not: 0xFFFF decodes to -0.004 C and
/// 0x03BC to 3.734 C, both inside every sensible band, so an ASUS ROG MAXIMUS
/// Z790 APEX (chip 0xD42B at config port 0x2E) reported a 3.7 C CPU and a
/// -0.0 C PCH as though they were measurements.
pub const EC_WINDOW_CHIPS: &[&str] = &["NCT6683D", "NCT6686D", "NCT6687D"];

/// Voltage sensors on NCT668x live in a contiguous 16-bit block.
pub const VOLTAGE_BLOCK_START: u16 = 0x120;
pub const VOLTAGE_BLOCK_COUNT: usize = 16;

/// Sensor LSB. The low 5 bits of every word read zero, and a BIOS change of
/// exactly -60 mV moved the VDDIO channel by exactly -480 raw counts, giving
/// 0.125 mV per count - a clean 1/8 mV step.
pub const SENSOR_STEP_VOLTS: f64 = 0.000125;

/// Several channels sit behind a 2:1 divider, so they decode at half the step.
pub const SENSOR_STEP_DIVIDED: f64 = SENSOR_STEP_VOLTS / 2.0;

/// rail key -> (sensor address, volts per count).
///
/// Confirmed on MSI B850MPOWER (NCT6687D, config port 0x4E, window 0x0A20).
/// Each entry was established against a known-good reading, not inferred:
///
/// ```text
///   CPU VDDIO  0x0126  changed in BIOS 1.47 -> 1.41 V, moving the channel
///                      0x2E60 -> 0x2C80, i.e. -480 counts for -60 mV, giving
///                      exactly 0.125 mV/count with no divider. Nothing else in
///                      the window moved with a clean scale in the right
///                      direction. Reads ~14 mV above setpoint at both points,
///                      matching HWiNFO's own offset.
///
///   VTT        0x0136  ranged 4.0880-4.0920 across an idle/load cycle, which
///                      halves to 2.0440-2.0460 -- HWiNFO's TM5 min AND max of
///                      2.044 / 2.046 exactly. Two matched endpoints fix both
///                      the scale and the absence of an offset.
///
///   Vcore      0x0128  the only load-tracking channel: 2.5520 -> 2.3240 under
///                      an all-core load. Halved, 1.1620-1.2760 V sits inside
///                      HWiNFO's TM5 range of 1.036-1.326 V, and the divider is
///                      the same one VTT and CPU NB/SoC independently confirm.
/// ```
///
/// The divider was not guessed: 0x0124 halves to 1.2060 V against HWiNFO's CPU
/// NB/SoC 1.206 V exactly, a second fixed rail agreeing with VTT.
///
/// NOTE: this mapping is BOARD-SPECIFIC. The index-to-rail assignment and the
/// per-channel dividers are chosen by the board vendor, so they must be
/// re-derived per model rather than assumed to carry over.
/// NOTE: the core rail is deliberately absent. 0x0128 is the VRM-side Vcore
/// measurement, but VDDCR_VDD is served from the CPU's own SVI3 telemetry in the
/// PM table instead, matching HWiNFO's "(SVI3 TFN)" reading. The two measure the
/// same rail in different places and differ by tens of millivolts.
pub const CONFIRMED_SENSORS: &[(&str, (u16, f64))] = &[
    ("vddio_mem", (0x126, SENSOR_STEP_VOLTS)),
    ("vtt", (0x136, SENSOR_STEP_DIVIDED)),
];

// Believable band for any board voltage sensor, used to shortlist candidates.
pub const CANDIDATE_MIN_VOLTS: f64 = 0.20;
pub const CANDIDATE_MAX_VOLTS: f64 = 6.00;

/// Why a sensor read did not happen: Python's `SuperIoUnavailable` (no
/// supported Super I/O responds, or none has been detected) or the `OSError`
/// a port access or the bus mutex raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SuperIoError {
    Unavailable(String),
    Io(HwError),
}

impl fmt::Display for SuperIoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SuperIoError::Unavailable(message) => f.write_str(message),
            SuperIoError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SuperIoError {}

impl From<HwError> for SuperIoError {
    fn from(error: HwError) -> Self {
        SuperIoError::Io(error)
    }
}

/// The named mutex actually taken for `name`. Unit tests take a private one
/// per test thread, so they neither wait on a monitoring tool running on the
/// machine nor on each other.
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

/// Take the ISA bus mutex with the default timeout.
fn isa_mutex() -> HwResult<NamedMutex> {
    NamedMutex::acquire(&bus_mutex_name(ISA_MUTEX_NAME), DEFAULT_MUTEX_TIMEOUT_MS)
}

/// What one pass over the configuration ports found.
struct Probe {
    found: Option<(u16, u16, &'static str, u16)>,
    declined: Vec<(u16, u16, &'static str)>,
}

/// Detect the Super I/O and read its sensor window. Read-only.
pub struct SuperIoReader {
    lock: Mutex<()>,
    pub config_port: Option<u16>,
    pub chip_id: Option<u16>,
    pub chip_name: Option<&'static str>,
    pub ec_base: Option<u16>,
    pub last_error: String,
}

impl Default for SuperIoReader {
    fn default() -> Self {
        Self::new()
    }
}

impl SuperIoReader {
    pub fn new() -> Self {
        SuperIoReader {
            lock: Mutex::new(()),
            config_port: None,
            chip_id: None,
            chip_name: None,
            ec_base: None,
            last_error: String::new(),
        }
    }

    // -- configuration-space helpers (caller holds the mutex) --------------

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
            let name = chip_id_name(chip_id)
                .or_else(|| chip_id_name(chip_id & 0xFFF0))
                .unwrap_or("unknown");
            if !EC_WINDOW_CHIPS.contains(&name) {
                // Keep the identity so the refusal can be diagnosed, but
                // leave ec_base unset: read_word must not address a window
                // this chip does not have. Carry on to the other port in
                // case the board answers on both.
                //
                // Recorded, not overwritten. An unpopulated config port can
                // answer with junk that is neither 0x0000 nor 0xFFFF, so
                // keeping the last decline would let that junk displace the
                // real chip: this board reports 0xD42B on 0x2E and 0x9090
                // on 0x4E, and 0x9090 is the one nobody can act on.
                declined.push((port, chip_id, name));
                continue;
            }
            return Ok(Probe { found: Some((port, chip_id, name, base)), declined });
        }
        Ok(Probe { found: None, declined })
    }

    /// Find a supported Super I/O and its sensor window.
    ///
    /// Returns `Ok(true)` on success; `last_error` explains an `Ok(false)`.
    /// `Err` is the bus mutex not being had, which the Python raised.
    pub fn detect(&mut self) -> HwResult<bool> {
        self.last_error = String::new();
        // Cleared up front so a second call cannot leave a window from a
        // previous detection addressable after this one declines the chip.
        self.ec_base = None;
        self.chip_id = None;
        self.chip_name = None;
        if !hw().is_driver_open() {
            self.last_error = "InpOut driver is not open".into();
            return Ok(false);
        }
        let probe = self.probe()?;
        if let Some(&(port, chip_id, name)) = probe.declined.first() {
            self.config_port = Some(port);
            self.chip_id = Some(chip_id);
            self.chip_name = Some(name);
        }
        if let Some((port, chip_id, name, base)) = probe.found {
            self.config_port = Some(port);
            self.chip_id = Some(chip_id);
            self.chip_name = Some(name);
            self.ec_base = Some(base);
            return Ok(true);
        }
        if !probe.declined.is_empty() {
            self.last_error = format!(
                "no Super I/O exposing the NCT668x sensor window; declined: {}",
                probe
                    .declined
                    .iter()
                    .map(|(port, chip_id, name)| format!("0x{chip_id:04X} ({name}) at 0x{port:02X}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        } else {
            self.last_error = "No supported Super I/O responded on 0x2E or 0x4E".into();
        }
        Ok(false)
    }

    // -- sensor window ------------------------------------------------------

    /// Address and read one sensor byte.
    ///
    /// The data port does not auto-increment on this chip, so every byte is
    /// addressed individually. The 0xFF preamble on the page register is
    /// required; without it every read returns the same byte.
    fn read_ec_byte_locked(base: u16, address: u16) -> HwResult<u8> {
        let io = hw();
        io.outb(base + EC_PAGE_OFFSET, EC_PAGE_SELECT)?;
        io.outb(base + EC_PAGE_OFFSET, ((address >> 8) & 0xFF) as u8)?;
        io.outb(base + EC_INDEX_OFFSET, (address & 0xFF) as u8)?;
        io.inb(base + EC_DATA_OFFSET)
    }

    pub fn read_bytes(&self, address: u16, count: usize) -> Result<Vec<u8>, SuperIoError> {
        let Some(base) = self.ec_base else {
            return Err(SuperIoError::Unavailable("Super I/O has not been detected".into()));
        };
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _bus = isa_mutex()?;
        (0..count)
            .map(|offset| Self::read_ec_byte_locked(base, address.wrapping_add(offset as u16)))
            .collect::<HwResult<Vec<u8>>>()
            .map_err(SuperIoError::Io)
    }

    /// Read a big-endian 16-bit sensor value.
    pub fn read_word(&self, address: u16) -> Result<u16, SuperIoError> {
        let bytes = self.read_bytes(address, 2)?;
        Ok(((bytes[0] as u16) << 8) | bytes[1] as u16)
    }

    /// Return `{address: raw word}` for the voltage sensor block
    /// (`VOLTAGE_BLOCK_START`, `VOLTAGE_BLOCK_COUNT` are the usual arguments).
    pub fn read_voltage_block(&self, start: u16, count: usize) -> BTreeMap<u16, u16> {
        let mut values = BTreeMap::new();
        for index in 0..count {
            let address = start.wrapping_add((index * 2) as u16);
            if let Ok(word) = self.read_word(address) {
                values.insert(address, word);
            }
        }
        values
    }
}

/// The detected reader, so repeat reads skip chip detection.
static DETECTED: Memo<Arc<SuperIoReader>> = Memo::new();

/// Decode a raw sensor word into volts (`SENSOR_STEP_VOLTS` is the usual step).
pub fn decode_sensor_volts(raw: u32, step: f64) -> f64 {
    (raw & 0xFFFF) as f64 * step
}

/// Read the confirmed Super I/O rails. Returns `{rail key: volts}`.
///
/// Anything decoding outside its believable range is dropped rather than
/// displayed, keeping this transport fail-closed like the other two.
pub fn read_board_rails() -> BTreeMap<&'static str, f64> {
    read_rails(CONFIRMED_SENSORS)
}

fn read_rails(sensors: &[(&'static str, (u16, f64))]) -> BTreeMap<&'static str, f64> {
    if sensors.is_empty() {
        return BTreeMap::new();
    }
    let reader = match DETECTED.get() {
        Some(reader) => reader,
        None => {
            let mut reader = SuperIoReader::new();
            match reader.detect() {
                Ok(true) => {}
                _ => return BTreeMap::new(),
            }
            // Detection unlocks and re-locks the configuration window; the
            // chip cannot move while the machine is running, so keep it.
            let reader = Arc::new(reader);
            DETECTED.set(reader.clone());
            reader
        }
    };
    let mut values = BTreeMap::new();
    for &(key, (address, step)) in sensors {
        let Some(rail) = rail_by_key(key) else {
            continue;
        };
        let Ok(word) = reader.read_word(address) else {
            continue;
        };
        if let Ok(volts) = validate_voltage(rail, decode_sensor_volts(word as u32, step)) {
            values.insert(key, volts);
        }
    }
    values
}

/// True when a decoded sensor could plausibly be a board rail.
pub fn is_candidate_voltage(volts: f64) -> bool {
    (CANDIDATE_MIN_VOLTS..=CANDIDATE_MAX_VOLTS).contains(&volts)
}

/// Test doubles for the Nuvoton NCT668x transport, shared with the modules
/// that build on it.
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;
    use crate::hardware::{Hardware, set_thread_hardware};
    use std::collections::HashMap;

    pub struct State {
        pub writes: Vec<(u16, u8)>,
        pub answer_on: Option<u16>,
        pub chip_id: u16,
        pub ec_base: u16,
        pub sensors: HashMap<u16, u8>,
        /// When set, a sensor byte nobody put there fails to read, the way
        /// the Python tests' fake reader raised `OSError` for it.
        pub missing_fails: bool,
        config_index: Option<u8>,
        page: Option<u8>,
        index: Option<u8>,
        pub driver_open: bool,
        pub unlocked: bool,
        /// Every sensor byte addressed through the data port, in order.
        pub byte_reads: Vec<u16>,
    }

    /// Super I/O that answers as an NCT6687D and records every write.
    pub struct FakeIo {
        pub state: Mutex<State>,
    }

    pub const CHIP_ID: u16 = 0xD592;
    pub const EC_BASE: u16 = 0x0A20;

    impl FakeIo {
        pub fn new(sensors: &[(u16, u8)], answer_on: Option<u16>) -> Self {
            FakeIo {
                state: Mutex::new(State {
                    writes: Vec::new(),
                    answer_on,
                    chip_id: CHIP_ID,
                    ec_base: EC_BASE,
                    sensors: sensors.iter().copied().collect(),
                    missing_fails: false,
                    config_index: None,
                    page: None,
                    index: None,
                    driver_open: true,
                    unlocked: false,
                    byte_reads: Vec::new(),
                }),
            }
        }

        /// An NCT6687D at 0x4E whose sensor words are `words` (big-endian).
        pub fn with_words(words: &[(u16, u16)]) -> Self {
            let mut bytes = Vec::new();
            for &(address, word) in words {
                bytes.push((address, (word >> 8) as u8));
                bytes.push((address + 1, (word & 0xFF) as u8));
            }
            let io = FakeIo::new(&bytes, Some(0x4E));
            io.state.lock().unwrap().missing_fails = true;
            io
        }

        pub fn chip(self, chip_id: u16) -> Self {
            self.state.lock().unwrap().chip_id = chip_id;
            self
        }

        pub fn install(self) -> Arc<FakeIo> {
            let shared = Arc::new(self);
            set_thread_hardware(Some(shared.clone()));
            shared
        }

        pub fn writes(&self) -> Vec<(u16, u8)> {
            self.state.lock().unwrap().writes.clone()
        }

        pub fn clear_writes(&self) {
            self.state.lock().unwrap().writes.clear();
        }

        pub fn byte_reads(&self) -> Vec<u16> {
            self.state.lock().unwrap().byte_reads.clone()
        }

        pub fn unlocked(&self) -> bool {
            self.state.lock().unwrap().unlocked
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
            if CONFIG_PORTS.contains(&(port.wrapping_sub(1))) {
                return Ok(());
            }
            if port == s.ec_base + EC_PAGE_OFFSET {
                s.page = Some(value);
                return Ok(());
            }
            if port == s.ec_base + EC_INDEX_OFFSET {
                s.index = Some(value);
            }
            Ok(())
        }

        fn inb(&self, port: u16) -> HwResult<u8> {
            let mut s = self.state.lock().unwrap();
            let config = port.wrapping_sub(1);
            if CONFIG_PORTS.contains(&config) {
                if Some(config) != s.answer_on {
                    return Ok(0xFF);
                }
                return Ok(match s.config_index {
                    Some(REG_CHIP_ID_HIGH) => (s.chip_id >> 8) as u8,
                    Some(REG_CHIP_ID_LOW) => (s.chip_id & 0xFF) as u8,
                    Some(REG_BASE_ADDRESS_HIGH) => (s.ec_base >> 8) as u8,
                    Some(REG_BASE_ADDRESS_LOW) => (s.ec_base & 0xFF) as u8,
                    _ => 0x00,
                });
            }
            if port == s.ec_base + EC_DATA_OFFSET {
                let address = ((s.page.unwrap_or(0) as u16) << 8) | s.index.unwrap_or(0) as u16;
                s.byte_reads.push(address);
                return match s.sensors.get(&address) {
                    Some(value) => Ok(*value),
                    None if s.missing_fails => {
                        Err(HwError(format!("no sensor at 0x{address:04X}")))
                    }
                    None => Ok(0x00),
                };
            }
            Ok(0x00)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{EC_BASE, FakeIo};
    use super::*;

    fn make_reader(io: FakeIo) -> (SuperIoReader, Arc<FakeIo>) {
        (SuperIoReader::new(), io.install())
    }

    fn default_io() -> FakeIo {
        FakeIo::new(&[], Some(0x4E))
    }

    fn assert_close(actual: f64, expected: f64, places: i32) {
        assert!(
            (actual - expected).abs() < 0.5 * 10f64.powi(-places),
            "{actual} != {expected} to {places} places"
        );
    }

    // -- DetectionTest -----------------------------------------------------

    #[test]
    fn detects_the_chip_and_sensor_window() {
        let (mut reader, _) = make_reader(default_io());
        assert!(reader.detect().unwrap());
        assert_eq!(reader.chip_id, Some(0xD592));
        assert_eq!(reader.chip_name, Some("NCT6687D"));
        assert_eq!(reader.ec_base, Some(0x0A20));
        assert_eq!(reader.config_port, Some(0x4E));
    }

    #[test]
    fn config_mode_is_always_exited() {
        let (mut reader, io) = make_reader(default_io());
        reader.detect().unwrap();
        assert!(!io.unlocked(), "chip was left unlocked");
        let locks: Vec<_> = io
            .writes()
            .into_iter()
            .filter(|(p, v)| CONFIG_PORTS.contains(p) && *v == NUVOTON_LOCK_BYTE)
            .collect();
        assert!(!locks.is_empty());
    }

    #[test]
    fn a_chip_without_this_sensor_window_is_declined() {
        // ASUS ROG MAXIMUS Z790 APEX answers 0xD42B on config port 0x2E. It is
        // an NCT679x part, and addressing the NCT668x window on one does not
        // fault: it returns 0xFFFF, which every temperature band accepts as
        // -0.004 C. Declining the chip is the only thing that stops that.
        let io = FakeIo::new(&[], Some(0x2E)).chip(0xD42B);
        let (mut reader, _) = make_reader(io);
        assert!(!reader.detect().unwrap());
        // Identity kept so the refusal can be diagnosed...
        assert_eq!(reader.chip_id, Some(0xD42B));
        assert!(reader.last_error.contains("0xD42B"));
        // ...but the window must not be addressable.
        assert_eq!(reader.ec_base, None);
        assert!(matches!(reader.read_word(0x100), Err(SuperIoError::Unavailable(_))));
    }

    #[test]
    fn declining_a_chip_still_relocks_the_config_window() {
        let io = FakeIo::new(&[], Some(0x2E)).chip(0xD42B);
        let (mut reader, io) = make_reader(io);
        reader.detect().unwrap();
        assert!(!io.unlocked());
    }

    #[test]
    fn detection_failure_is_reported_not_raised() {
        let io = FakeIo::new(&[], None).chip(0xFFFF);
        let (mut reader, _) = make_reader(io);
        assert!(!reader.detect().unwrap());
        assert!(!reader.last_error.is_empty());
    }

    #[test]
    fn closed_driver_is_reported() {
        let io = default_io();
        io.state.lock().unwrap().driver_open = false;
        let (mut reader, _) = make_reader(io);
        assert!(!reader.detect().unwrap());
        assert!(reader.last_error.to_lowercase().contains("driver"));
    }

    // -- SensorReadTest ----------------------------------------------------

    #[test]
    fn page_preamble_precedes_the_bank() {
        // Without the 0xFF preamble every register returns the same byte.
        let (mut reader, io) = make_reader(default_io());
        reader.detect().unwrap();
        io.clear_writes();
        reader.read_word(0x126).unwrap();
        let page_writes: Vec<u8> = io
            .writes()
            .into_iter()
            .filter(|(p, _)| *p == EC_BASE + EC_PAGE_OFFSET)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(page_writes[0], EC_PAGE_SELECT);
        assert_eq!(page_writes[1], 0x01);
    }

    #[test]
    fn each_byte_is_addressed_individually() {
        // The data port does not auto-increment on this chip.
        let (mut reader, io) = make_reader(default_io());
        reader.detect().unwrap();
        io.clear_writes();
        reader.read_word(0x126).unwrap();
        let index_writes: Vec<u8> = io
            .writes()
            .into_iter()
            .filter(|(p, _)| *p == EC_BASE + EC_INDEX_OFFSET)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(index_writes, vec![0x26, 0x27]);
    }

    #[test]
    fn word_is_big_endian() {
        let (mut reader, _) = make_reader(FakeIo::new(&[(0x126, 0x2C), (0x127, 0x80)], Some(0x4E)));
        reader.detect().unwrap();
        assert_eq!(reader.read_word(0x126).unwrap(), 0x2C80);
    }

    #[test]
    fn reading_before_detection_raises() {
        let (reader, _) = make_reader(default_io());
        assert!(matches!(reader.read_word(0x126), Err(SuperIoError::Unavailable(_))));
    }

    // -- ReadOnlyTest ------------------------------------------------------

    #[test]
    fn module_has_no_sensor_write_primitive() {
        // The module's own code: everything above its test doubles, whose
        // stand-in driver has to implement the trait's physical-memory write.
        let source = include_str!("superio_lpc.rs");
        let module = source.split(concat!("mod ", "test_support")).next().unwrap();
        // This chip drives fan control; a write primitive must be absent.
        for forbidden in [
            concat!("fn ", "write_word"),
            concat!("fn ", "write_sensor"),
            concat!("fn ", "set_"),
        ] {
            assert!(!module.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn only_selector_ports_are_written_during_a_read() {
        let (mut reader, io) = make_reader(default_io());
        reader.detect().unwrap();
        io.clear_writes();
        reader.read_word(0x126).unwrap();
        let allowed = [EC_BASE + EC_PAGE_OFFSET, EC_BASE + EC_INDEX_OFFSET];
        for (port, _) in io.writes() {
            assert!(allowed.contains(&port), "read path wrote to an unexpected port");
        }
        assert!(
            !io.writes().iter().any(|(p, _)| *p == EC_BASE + EC_DATA_OFFSET),
            "the data port must never be written"
        );
    }

    // -- ConfirmedRailTest -------------------------------------------------

    fn confirmed(key: &str) -> (u16, f64) {
        CONFIRMED_SENSORS.iter().find(|(k, _)| *k == key).unwrap().1
    }

    #[test]
    fn vddio_scale_matches_the_bios_calibration() {
        // CPU VDDIO 1.47 -> 1.41 V moved 0x0126 by -480 counts, giving exactly
        // 0.125 mV per count with no divider.
        let (address, step) = confirmed("vddio_mem");
        assert_eq!(address, 0x126);
        assert_close(step, 0.000125, 7);
        assert_close(SENSOR_STEP_VOLTS, 0.000125, 7);
    }

    #[test]
    fn vddio_readings_decode_to_the_bios_values() {
        assert_close(decode_sensor_volts(0x2E60, SENSOR_STEP_VOLTS), 1.484, 4);
        assert_close(decode_sensor_volts(0x2C80, SENSOR_STEP_VOLTS), 1.424, 4);
        // The delta must equal the BIOS change exactly.
        assert_close(
            decode_sensor_volts(0x2C80, SENSOR_STEP_VOLTS) - decode_sensor_volts(0x2E60, SENSOR_STEP_VOLTS),
            -0.060,
            6,
        );
    }

    #[test]
    fn vtt_matches_both_hwinfo_endpoints() {
        // HWiNFO over a TM5 run: VTT min 2.044, max 2.046. The channel ranged
        // 4.0880-4.0920 raw, which halves to exactly those two values.
        let (address, step) = confirmed("vtt");
        assert_eq!(address, 0x136);
        assert_close(step, SENSOR_STEP_DIVIDED, 7);
        assert_close(decode_sensor_volts(0x7FC0, step), 2.0440, 4);
        assert_close(decode_sensor_volts(0x7FE0, step), 2.0460, 4);
    }

    #[test]
    fn core_rail_is_not_served_from_this_chip() {
        // 0x0128 is the VRM-side Vcore measurement and does track load
        // (2.5520 -> 2.3240 raw, i.e. 1.2760 -> 1.1620 V halved), but
        // VDDCR_VDD is served from the CPU's own SVI3 telemetry in the PM
        // table so it matches HWiNFO's "(SVI3 TFN)" reading instead.
        assert!(!CONFIRMED_SENSORS.iter().any(|(k, _)| *k == "vddcr_vdd"));
        assert_close(decode_sensor_volts(0x4FC0, SENSOR_STEP_DIVIDED), 1.2760, 3);
    }

    #[test]
    fn divider_reproduces_the_cpu_nb_soc_cross_check() {
        // 0x0124 halves to CPU NB/SoC 1.206 V exactly. That second fixed rail
        // is what establishes the divider rather than fitting it to Vcore.
        assert_close(decode_sensor_volts(0x4B60, SENSOR_STEP_DIVIDED), 1.2060, 4);
    }

    #[test]
    fn read_board_rails_returns_the_decoded_rails() {
        FakeIo::new(
            &[
                (0x126, 0x2C), (0x127, 0x80), // VDDIO 1.424
                (0x136, 0x7F), (0x137, 0xE0), // VTT   2.046
            ],
            Some(0x4E),
        )
        .install();
        let rails = read_board_rails();
        assert_close(rails["vddio_mem"], 1.424, 4);
        assert_close(rails["vtt"], 2.046, 4);
    }

    #[test]
    fn out_of_range_reading_is_dropped() {
        FakeIo::new(&[(0x126, 0xFF), (0x127, 0xFF)], Some(0x4E)).install();
        let rails = read_board_rails();
        assert!(!rails.contains_key("vddio_mem"));
    }

    #[test]
    fn detection_failure_yields_no_rails() {
        FakeIo::new(&[], None).install();
        assert!(read_board_rails().is_empty());
    }

    #[test]
    fn only_the_board_sensor_rails_are_claimed() {
        let keys: Vec<&str> = CONFIRMED_SENSORS.iter().map(|(k, _)| *k).collect();
        assert_eq!(keys, vec!["vddio_mem", "vtt"]);
    }
}
