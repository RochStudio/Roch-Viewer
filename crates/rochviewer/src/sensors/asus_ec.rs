//! Read-only ACPI embedded-controller access, for board sensors nothing else
//! reports.
//!
//! Fifth and last transport in this project, and the only one that shares its
//! device with the operating system. The ACPI EC is driven by Windows' own ACPI
//! driver; this module reads bytes out of its RAM alongside it, which is what
//! LibreHardwareMonitor and HWiNFO do and why they take the same mutex.
//!
//! It exists because a handful of readings live nowhere else. On the Z790 bench
//! HWiNFO's "ASUS EC" section carries the VRM temperature, and the Super I/O this
//! project already reads carries no VRM channel at all -- the row was blank not
//! because the board lacks the sensor but because it is on this controller.
//!
//! SAFETY - this module reads and nothing else:
//!
//!   * The EC has two commands that matter: RD_EC (0x80) and WR_EC (0x81). Only
//!     RD_EC is defined here. There is no write-command constant, no data-write
//!     primitive, and no code path that could assemble one, so no EC register can
//!     be modified through this module. The capability is absent rather than
//!     merely unused, which matters more here than anywhere else in this project:
//!     on many boards the EC owns fan control and charging.
//!   * The writes that do happen are the read protocol itself -- the RD_EC
//!     command byte and the register address. Both select *what to read*, exactly
//!     as writing an index into an SMBus HOST_COMMAND does.
//!   * Every wait is bounded. A controller that stops answering makes this module
//!     give up and report nothing; it can never spin holding the EC.
//!   * Access is serialised on the named mutex the mainstream monitoring tools
//!     take for the embedded controller, so a read cannot land between another
//!     tool's command byte and its data byte.
//!   * The status register is checked before each step rather than assumed, so a
//!     transaction left half-finished by anyone else is detected rather than
//!     completed with someone else's address.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::hardware::lowlevel_io::{DEFAULT_MUTEX_TIMEOUT_MS, NamedMutex};
use crate::hardware::{HwError, HwResult, hw};
use crate::memo::Memo;

/// The mutex mainstream monitoring tools take before driving the EC.
pub const EC_MUTEX_NAME: &str = "Global\\Access_EC";

// Standard ACPI embedded-controller ports.
pub const EC_DATA_PORT: u16 = 0x62;
pub const EC_COMMAND_PORT: u16 = 0x66;

// Status register bits, read from the command port.
pub const STATUS_OUTPUT_FULL: u8 = 1 << 0; // OBF: a byte is waiting to be read
pub const STATUS_INPUT_FULL: u8 = 1 << 1; // IBF: the controller has not taken ours yet

/// The read command. Its counterpart WR_EC is deliberately absent; see the
/// safety note above.
pub const COMMAND_READ: u8 = 0x80;

// Bounded waits. The EC is slow by design and answers in microseconds when
// idle, but it can be busy with the OS driver, so the budget is generous and
// the polling interval short.
pub const STATUS_TIMEOUT_S: f64 = 0.02;
pub const STATUS_POLL_S: f64 = 0.0001;

/// Why an EC read did not happen: Python's `EcUnavailable` (the controller
/// cannot be driven), the `TimeoutError` of a status bit that never came, or
/// the `OSError` a port access or the EC mutex raised.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EcError {
    Unavailable(String),
    Timeout(String),
    Io(HwError),
}

impl fmt::Display for EcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EcError::Unavailable(message) | EcError::Timeout(message) => f.write_str(message),
            EcError::Io(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for EcError {}

impl From<HwError> for EcError {
    fn from(error: HwError) -> Self {
        EcError::Io(error)
    }
}

/// The named mutex actually taken; unit tests take a private one per thread.
#[cfg(not(test))]
fn ec_mutex_name() -> String {
    EC_MUTEX_NAME.to_string()
}

#[cfg(test)]
fn ec_mutex_name() -> String {
    crate::sensors::superio_lpc::bus_mutex_name(EC_MUTEX_NAME)
}

/// Serialised, read-only ACPI EC byte reader.
pub struct AcpiEcReader {
    lock: Mutex<()>,
    timeout: f64,
    pub last_error: String,
}

impl Default for AcpiEcReader {
    fn default() -> Self {
        Self::new()
    }
}

impl AcpiEcReader {
    /// A reader with the standard status budget, `STATUS_TIMEOUT_S`.
    pub fn new() -> Self {
        AcpiEcReader { lock: Mutex::new(()), timeout: STATUS_TIMEOUT_S, last_error: String::new() }
    }

    /// A reader with its own status budget, in seconds; refused outside
    /// (0, 0.25] so no wait can hold the EC for long.
    pub fn with_timeout(timeout: f64) -> Result<Self, String> {
        if !(0.0 < timeout && timeout <= 0.25) {
            return Err("EC timeout must be within (0, 0.25]".into());
        }
        Ok(AcpiEcReader { timeout, ..Self::new() })
    }

    pub fn is_driver_open(&self) -> bool {
        hw().is_driver_open()
    }

    /// Poll the status register until one bit reaches a state.
    fn wait_for(&self, bit: u8, is_set: bool) -> Result<u8, EcError> {
        let io = hw();
        let deadline = Instant::now() + Duration::from_secs_f64(self.timeout);
        loop {
            let status = io.inb(EC_COMMAND_PORT)?;
            if (status & bit != 0) == is_set {
                return Ok(status);
            }
            if Instant::now() > deadline {
                return Err(EcError::Timeout(format!(
                    "EC status 0x{status:02X} never reached {} on bit 0x{bit:02X}",
                    if is_set { "set" } else { "clear" }
                )));
            }
            std::thread::sleep(Duration::from_secs_f64(STATUS_POLL_S));
        }
    }

    /// Discard a byte another owner left in the output buffer.
    ///
    /// Starting a read with OBF already set would return that byte as though
    /// it were ours.
    fn drain(&self) -> HwResult<()> {
        let io = hw();
        let status = io.inb(EC_COMMAND_PORT)?;
        if status & STATUS_OUTPUT_FULL != 0 {
            io.inb(EC_DATA_PORT)?;
        }
        Ok(())
    }

    fn read_locked(&self, register: u8) -> Result<u8, EcError> {
        let io = hw();
        self.drain()?;
        self.wait_for(STATUS_INPUT_FULL, false)?;
        io.outb(EC_COMMAND_PORT, COMMAND_READ)?;
        self.wait_for(STATUS_INPUT_FULL, false)?;
        io.outb(EC_DATA_PORT, register)?;
        self.wait_for(STATUS_OUTPUT_FULL, true)?;
        Ok(io.inb(EC_DATA_PORT)?)
    }

    fn ec_mutex() -> HwResult<NamedMutex> {
        NamedMutex::acquire(&ec_mutex_name(), DEFAULT_MUTEX_TIMEOUT_MS)
    }

    /// Read one byte of EC RAM. Fails on timeout; never returns partial.
    pub fn read_byte(&self, register: u8) -> Result<u8, EcError> {
        if !hw().is_driver_open() {
            return Err(EcError::Unavailable("InpOut driver is not open".into()));
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ec = Self::ec_mutex()?;
        self.read_locked(register)
    }

    /// Read several EC registers under one acquisition of the mutex.
    pub fn read_bytes(&self, registers: &[u8]) -> Result<Vec<u8>, EcError> {
        if !hw().is_driver_open() {
            return Err(EcError::Unavailable("InpOut driver is not open".into()));
        }
        let _guard = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _ec = Self::ec_mutex()?;
        registers.iter().map(|&register| self.read_locked(register)).collect()
    }
}

/// Decode an EC temperature byte into degrees Celsius.
///
/// Whole signed degrees, the same encoding the Super I/O uses.
pub fn decode_temperature(raw: u32) -> i32 {
    let raw = (raw & 0xFF) as i32;
    if raw > 127 { raw - 256 } else { raw }
}

// --- Board identity.
//
// The EC protocol above is the ACPI standard and works on any board. What is
// in EC RAM is not: the register map is the board vendor's, so reading 0x33 on
// a machine from another vendor returns whatever that vendor put there, and a
// byte between 20 and 70 would pass the band below and be printed as a VRM
// temperature. That is exactly the failure this project already hit once, when
// an MSI Super I/O map was applied to an ASUS chip and produced a 3.7 C CPU.
//
// So the map is gated on the vendor rather than trusted everywhere. The entry
// below was confirmed on one ASUS board; other ASUS boards are believed to
// share this layout, which is why the gate is the vendor and not the model,
// and that belief is the weakest link here rather than the decode.
pub const BOARD_VENDOR_PREFIXES: &[&str] = &["ASUS"];

/// Whether this board's vendor uses the register map below.
pub fn is_asus_board(board_name: &str) -> bool {
    let name = board_name.trim().to_uppercase();
    BOARD_VENDOR_PREFIXES.iter().any(|prefix| name.contains(prefix))
}

// --- Board sensors.

/// key -> (EC register, min C, max C).
///
/// Confirmed on ASUS ROG MAXIMUS Z790 APEX against HWiNFO's own "ASUS EC"
/// section, and then through an idle -> all-core load -> idle cycle, because a
/// byte that happens to read 40 proves nothing on its own:
///
/// ```text
///   VRM  0x33  40 C idle, 43 C under an all-core load, 40 C recovered.
///              HWiNFO's ASUS EC VRM read 40 C with a 44 C maximum over its own
///              window: the same value at rest and the same ceiling loaded.
/// ```
///
/// The size of the response is what identifies it, not just the value. Four
/// other registers -- 0x30, 0x42, 0x53 and 0x59 -- swing 33 C to 65 C and back
/// across the same cycle, which is a die sensor mirrored four times. A VRM
/// carries far more thermal mass than the die and warms a few degrees where the
/// die swings thirty, so a channel sitting at 40 C and moving 3 C is the one
/// that is measuring the regulator.
pub const CONFIRMED_EC_TEMPERATURES: &[(&str, (u8, f64, f64))] = &[("vrm", (0x33, -20.0, 125.0))];

/// The detected reader, kept so a refusal is not re-tried on every refresh.
static READER: Memo<Option<Arc<AcpiEcReader>>> = Memo::new();

/// Return the reading, or `None` when it cannot be that sensor.
pub fn validate_temperature(key: &str, celsius: f64) -> Option<f64> {
    let &(_, (_register, minimum, maximum)) =
        CONFIRMED_EC_TEMPERATURES.iter().find(|(k, _)| *k == key)?;
    (minimum <= celsius && celsius <= maximum).then_some(celsius)
}

/// Return a working reader, or `None`. Resolved once.
fn detected_reader() -> Option<Arc<AcpiEcReader>> {
    READER.get_or_init(|| {
        let reader = AcpiEcReader::new();
        if !reader.is_driver_open() {
            return None;
        }
        // Prove the controller answers before it is trusted, so a board whose
        // EC does not respond costs one probe rather than one per refresh.
        let (_key, (register, _min, _max)) = CONFIRMED_EC_TEMPERATURES[0];
        reader.read_byte(register).ok()?;
        Some(Arc::new(reader))
    })
}

/// Return `{sensor key: celsius}` for the confirmed EC sensors.
pub fn read_temperatures() -> BTreeMap<&'static str, f64> {
    let sensors = CONFIRMED_EC_TEMPERATURES;
    if sensors.is_empty() {
        return BTreeMap::new();
    }
    let Some(reader) = detected_reader() else {
        return BTreeMap::new();
    };
    let mut values = BTreeMap::new();
    for &(key, (register, _minimum, _maximum)) in sensors {
        let Ok(raw) = reader.read_byte(register) else {
            continue;
        };
        let celsius = decode_temperature(raw as u32);
        if let Some(checked) = validate_temperature(key, celsius as f64) {
            values.insert(key, checked);
        }
    }
    values
}

/// Format one EC sensor for a table row, or `None` when unavailable.
pub fn temperature_text(key: &str) -> Option<String> {
    read_temperatures().get(key).map(|celsius| format!("{celsius:.1} °C"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{Hardware, set_thread_hardware};
    use std::collections::HashMap;

    struct State {
        ram: HashMap<u8, u8>,
        writes: Vec<(u16, u8)>,
        driver_open: bool,
        stuck: bool,
        status: u8,
        expect_address: bool,
        pending: Option<u8>,
        status_reads: usize,
    }

    /// An embedded controller that answers the standard read handshake.
    struct FakeEc {
        state: Mutex<State>,
    }

    impl FakeEc {
        fn new(ram: &[(u8, u8)]) -> Self {
            FakeEc {
                state: Mutex::new(State {
                    ram: ram.iter().copied().collect(),
                    writes: Vec::new(),
                    driver_open: true,
                    stuck: false,
                    status: 0x00,
                    expect_address: false,
                    pending: None,
                    status_reads: 0,
                }),
            }
        }

        fn stuck(self) -> Self {
            self.state.lock().unwrap().stuck = true;
            self
        }

        fn output_full_at_start(self) -> Self {
            self.state.lock().unwrap().status = STATUS_OUTPUT_FULL;
            self
        }

        fn driver_closed(self) -> Self {
            self.state.lock().unwrap().driver_open = false;
            self
        }

        fn install(self) -> Arc<FakeEc> {
            let shared = Arc::new(self);
            set_thread_hardware(Some(shared.clone()));
            shared
        }

        fn writes(&self) -> Vec<(u16, u8)> {
            self.state.lock().unwrap().writes.clone()
        }
    }

    impl Hardware for FakeEc {
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

        fn inb(&self, port: u16) -> HwResult<u8> {
            let mut s = self.state.lock().unwrap();
            if port == EC_COMMAND_PORT {
                s.status_reads += 1;
                return Ok(if s.stuck { 0xFF } else { s.status });
            }
            if port == EC_DATA_PORT {
                s.status &= !STATUS_OUTPUT_FULL;
                return Ok(s.pending.take().unwrap_or(0x00));
            }
            Ok(0x00)
        }

        fn outb(&self, port: u16, value: u8) -> HwResult<()> {
            let mut s = self.state.lock().unwrap();
            s.writes.push((port, value));
            if port == EC_COMMAND_PORT && value == COMMAND_READ {
                s.expect_address = true;
                return Ok(());
            }
            if port == EC_DATA_PORT && s.expect_address {
                s.expect_address = false;
                s.pending = Some(*s.ram.get(&value).unwrap_or(&0x00));
                s.status |= STATUS_OUTPUT_FULL;
            }
            Ok(())
        }
    }

    fn make_reader(io: FakeEc) -> (AcpiEcReader, Arc<FakeEc>) {
        (AcpiEcReader::new(), io.install())
    }

    // -- ProtocolTest ------------------------------------------------------

    #[test]
    fn a_register_is_read_through_the_standard_handshake() {
        let (reader, io) = make_reader(FakeEc::new(&[(0x33, 40)]));
        assert_eq!(reader.read_byte(0x33).unwrap(), 40);
        assert_eq!(io.writes(), vec![(EC_COMMAND_PORT, COMMAND_READ), (EC_DATA_PORT, 0x33)]);
    }

    #[test]
    fn a_byte_left_by_another_owner_is_discarded() {
        // Starting with OBF set would otherwise return someone else's byte as
        // though it were the answer to our address.
        let (reader, _) = make_reader(FakeEc::new(&[(0x33, 40)]).output_full_at_start());
        assert_eq!(reader.read_byte(0x33).unwrap(), 40);
    }

    #[test]
    fn a_controller_that_stops_answering_times_out() {
        let (reader, _) = make_reader(FakeEc::new(&[(0x33, 40)]).stuck());
        assert!(matches!(reader.read_byte(0x33), Err(EcError::Timeout(_))));
    }

    #[test]
    fn no_driver_is_reported_not_raised_as_something_else() {
        let (reader, _) = make_reader(FakeEc::new(&[(0x33, 40)]).driver_closed());
        assert!(matches!(reader.read_byte(0x33), Err(EcError::Unavailable(_))));
    }

    #[test]
    fn several_registers_read_under_one_lock() {
        let (reader, _) = make_reader(FakeEc::new(&[(0x33, 40), (0x30, 33)]));
        assert_eq!(reader.read_bytes(&[0x33, 0x30]).unwrap(), vec![40, 33]);
    }

    #[test]
    fn an_unreasonable_timeout_is_refused() {
        assert_eq!(
            AcpiEcReader::with_timeout(5.0).err().as_deref(),
            Some("EC timeout must be within (0, 0.25]")
        );
    }

    // -- ReadOnlyContractTest: the EC owns fan control on many boards; this
    // module must not write.

    /// The module's code, with comments and this test module left out: the
    /// doc comment names WR_EC precisely to explain why it is absent, and a
    /// test that forbade the word would forbid saying so.
    fn module_code() -> String {
        let source = include_str!("asus_ec.rs");
        let module = source.split(concat!("mod ", "tests {")).next().unwrap();
        module
            .lines()
            .map(|line| line.split("//").next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn no_write_command_is_defined() {
        let code = module_code();
        assert!(!code.contains(concat!("0x", "81")), "WR_EC must not be defined");
        for line in code.lines() {
            let line = line.trim();
            if line.starts_with("pub const") || line.starts_with("const") || line.starts_with("static") {
                assert!(!line.to_lowercase().contains("write"), "{line}");
            }
        }
    }

    #[test]
    fn no_method_can_write_a_register() {
        for line in module_code().lines() {
            if line.contains("fn ") {
                assert!(!line.to_lowercase().contains("write"), "{line}");
            }
        }
    }

    #[test]
    fn only_the_command_and_data_ports_are_written() {
        let (reader, io) = make_reader(FakeEc::new(&[(0x33, 40)]));
        reader.read_byte(0x33).unwrap();
        let mut ports: Vec<u16> = io.writes().iter().map(|(port, _)| *port).collect();
        ports.sort();
        ports.dedup();
        let mut expected = vec![EC_COMMAND_PORT, EC_DATA_PORT];
        expected.sort();
        assert_eq!(ports, expected);
    }

    #[test]
    fn the_only_command_byte_written_is_the_read_command() {
        let (reader, io) = make_reader(FakeEc::new(&[(0x33, 40)]));
        reader.read_byte(0x33).unwrap();
        let commands: Vec<u8> = io
            .writes()
            .into_iter()
            .filter(|(port, _)| *port == EC_COMMAND_PORT)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(commands, vec![COMMAND_READ]);
    }

    // -- BoardGateTest -----------------------------------------------------

    #[test]
    fn an_asus_board_is_recognised() {
        assert!(is_asus_board("ASUSTeK COMPUTER INC. ROG MAXIMUS Z790 APEX (Rev 1.xx)"));
        assert!(is_asus_board("asus rog strix"));
    }

    #[test]
    fn another_vendor_is_not() {
        // The register map is ASUS's. On an MSI board 0x33 holds something
        // else, and a byte between 20 and 70 would print as a VRM temperature.
        // (The Python's `None` case is the empty name here.)
        for name in [
            "Micro-Star International Co., Ltd. B850MPOWER",
            "Gigabyte Technology Co., Ltd.",
            "ASRock Z790",
            "",
        ] {
            assert!(!is_asus_board(name), "{name}");
        }
    }

    // -- VrmMappingTest ----------------------------------------------------

    #[test]
    fn the_confirmed_reading_decodes_to_what_hwinfo_showed() {
        let (key, (register, _minimum, _maximum)) = CONFIRMED_EC_TEMPERATURES[0];
        assert_eq!(key, "vrm");
        assert_eq!(register, 0x33);
        assert_eq!(decode_temperature(40), 40);
    }

    #[test]
    fn the_vrm_is_read_from_its_register() {
        FakeEc::new(&[(0x33, 43)]).install();
        let values = read_temperatures();
        assert_eq!(values, BTreeMap::from([("vrm", 43.0)]));
    }

    #[test]
    fn a_reading_outside_the_band_is_dropped() {
        assert_eq!(validate_temperature("vrm", 200.0), None);
        assert_eq!(validate_temperature("vrm", -100.0), None);
        assert_eq!(validate_temperature("vrm", 40.0), Some(40.0));
    }

    #[test]
    fn an_unknown_sensor_is_dropped() {
        assert_eq!(validate_temperature("nonexistent", 40.0), None);
    }

    #[test]
    fn a_negative_reading_is_signed() {
        assert_eq!(decode_temperature(0xFF), -1);
    }

    #[test]
    fn a_dead_controller_is_asked_once() {
        // Detection drives the EC and Summary refreshes every second.
        let io = FakeEc::new(&[]).stuck().install();
        assert!(read_temperatures().is_empty());
        let probed = io.state.lock().unwrap().status_reads;
        assert!(probed > 0);
        for _ in 0..4 {
            assert!(read_temperatures().is_empty());
        }
        assert_eq!(io.state.lock().unwrap().status_reads, probed);
    }

    #[test]
    fn temperature_text_carries_one_decimal_and_a_unit() {
        FakeEc::new(&[(0x33, 40)]).install();
        assert_eq!(temperature_text("vrm").as_deref(), Some("40.0 °C"));
        assert_eq!(temperature_text("cpu"), None);
    }
}
