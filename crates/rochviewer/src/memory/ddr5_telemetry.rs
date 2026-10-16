// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.

//! Per-DIMM PMIC telemetry: measured rails, input voltage and power.
//!
//! What [`ddr5_pmic`](super::ddr5_pmic) already reads are the VID registers --
//! what the rails are *set* to. This module reads what the PMIC's ADC
//! *measures*, which is a different thing and differs between two modules on
//! the same board setting.
//!
//! Getting a measurement costs one write. The ADC is a multiplexer: R30h
//! selects a channel, JESD301-2 requires ~9 ms to settle, and R31h then holds
//! the sample. R30h is a selector and carries no rail setting -- see the SAFETY
//! note in [`intel_pch_smbus`](crate::intel::intel_pch_smbus), where it is one
//! of exactly two writable targets and the rail registers are none of them.
//!
//! Decode follows ZenStates-Core's Ddr5PmicReader/Ddr5PmicDecoder, and was
//! checked against ZenTimings 1.43 on the bench: current mode (R1Bh bit 6
//! clear), limits 6000/6000/1250 mA from R20h, raws 3/1/0 -> 0.2405 W, inside
//! the 0.242-0.276 W the same window was reporting.
//!
//! This module also carries the SMBus transport contract both host
//! controllers implement ([`SmbusReader`]) and the dispatcher that picks one
//! ([`default_smbus_backend`]), since every reader of the DDR5 devices goes
//! through them. In the Python the contract was duck typing: the Intel
//! transport's methods were written with "the signature matches the AM5
//! transport's so one caller can drive either bus".

use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use crate::hardware::lowlevel_io::NamedMutex;
use crate::memo::Memo;
use crate::platform_profiles::{self, Platform};

// ---------------------------------------------------------------------------
// The SMBus transport contract
// ---------------------------------------------------------------------------

/// Why an SMBus call did not produce a value.
///
/// The variants are the Python exception classes the callers distinguished:
/// most callers catch `(OSError, TimeoutError, ValueError)` and let anything
/// else escape to an outer `except Exception`, so the kind is kept rather than
/// flattened into one message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmbusError {
    /// `ValueError`: refused before the bus was driven (an address outside
    /// an allowlist, an unknown controller offset, an out-of-range argument).
    Refused(String),
    /// `OSError`: the transfer, the port access or the mutex failed.
    Bus(String),
    /// `TimeoutError`: the controller stayed busy or never completed.
    Timeout(String),
    /// `SmbusUnavailable` (a `RuntimeError`): there is no controller to drive.
    Unavailable(String),
    /// Anything else the Python did not catch where the transfer errors were
    /// caught -- a reader without the method asked for, chiefly.
    Other(String),
}

impl SmbusError {
    /// `except (OSError, TimeoutError)`. `TimeoutError` is an `OSError`
    /// subclass in Python, so this is also what a bare `except OSError` caught.
    pub fn is_io(&self) -> bool {
        matches!(self, SmbusError::Bus(_) | SmbusError::Timeout(_))
    }

    /// `except (OSError, TimeoutError, ValueError)`, the usual catch.
    pub fn is_transfer(&self) -> bool {
        matches!(self, SmbusError::Refused(_) | SmbusError::Bus(_) | SmbusError::Timeout(_))
    }

    pub fn message(&self) -> &str {
        match self {
            SmbusError::Refused(m)
            | SmbusError::Bus(m)
            | SmbusError::Timeout(m)
            | SmbusError::Unavailable(m)
            | SmbusError::Other(m) => m,
        }
    }
}

impl fmt::Display for SmbusError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for SmbusError {}

/// SPD bytes as `read_spd` returns them: `{offset: byte}`, with a byte that
/// did not read simply absent.
pub type SpdBytes = BTreeMap<u32, u8>;

fn missing(method: &str) -> SmbusError {
    SmbusError::Other(format!("reader has no attribute '{method}'"))
}

/// One SMBus host controller, as the DDR5/DDR4 device readers drive it.
///
/// Implemented by [`FchSmbusReader`](crate::amd::fch_smbus::FchSmbusReader)
/// and [`PchSmbusReader`](crate::intel::intel_pch_smbus::PchSmbusReader), and
/// by the tests' stand-ins. A method a transport does not have answers
/// [`SmbusError::Other`], as calling a missing method raised `AttributeError`
/// in the Python: the AMD FCH has no Word Data path and no DDR4 page selector.
pub trait SmbusReader: Send + Sync {
    fn is_driver_open(&self) -> bool;

    /// Read one DDR5 device register with the Read Byte Data protocol.
    fn read_byte(&self, _address: u8, _register: u8, _controller: u8) -> Result<u8, SmbusError> {
        Err(missing("read_byte"))
    }

    /// Write one byte, to an allowlisted selector only.
    fn write_byte(&self, _address: u8, _register: u8, _value: u8, _controller: u8) -> Result<(), SmbusError> {
        Err(missing("write_byte"))
    }

    /// Read SPD5 hub EEPROM bytes across page boundaries.
    fn read_spd(&self, _address: u8, _offset: i64, _length: i64, _controller: u8) -> Result<SpdBytes, SmbusError> {
        Err(missing("read_spd"))
    }

    /// Read an EE1004 DDR4 SPD range (absolute offsets 0-511).
    fn read_ddr4_spd(&self, _address: u8, _offset: i64, _length: i64, _controller: u8) -> Result<SpdBytes, SmbusError> {
        Err(missing("read_ddr4_spd"))
    }

    /// True when a device answers at `address`. The transports read register
    /// 0 and report any refusal, timeout or bus error as absent.
    fn probe_address(&self, address: u8, controller: u8) -> bool {
        self.read_byte(address, 0x00, controller).is_ok()
    }

    /// Read a register with the Read Word protocol, bytes in wire order.
    fn read_word_bytes(&self, _address: u8, _register: u8) -> Result<(u8, u8), SmbusError> {
        Err(missing("read_word_bytes"))
    }

    /// The JC-42.4 sensor addresses that answered, in slot order.
    fn responding_addresses(&self, _register: u8) -> Result<Vec<u8>, SmbusError> {
        Err(missing("responding_addresses"))
    }
}

/// What a `reader_factory` argument is: build (or hand back) a reader. The
/// Python passed the transport class itself, whose constructor could raise.
pub type ReaderFactory<'a> = &'a dyn Fn() -> Result<Arc<dyn SmbusReader>, SmbusError>;

/// A held bus mutex; released when dropped.
pub type BusGuard = Box<dyn std::any::Any + Send>;

/// The serialising mutex a transport takes around each transaction.
pub trait BusMutex: Send + Sync {
    fn acquire(&self) -> Result<BusGuard, SmbusError>;
}

/// A named Windows mutex, shared with the other monitoring tools.
pub struct NamedBusMutex {
    pub name: &'static str,
    pub timeout_ms: u32,
}

impl NamedBusMutex {
    /// The Python `NamedMutex(name)`: five-second timeout.
    pub const fn new(name: &'static str) -> Self {
        NamedBusMutex { name, timeout_ms: 5000 }
    }
}

impl BusMutex for NamedBusMutex {
    fn acquire(&self) -> Result<BusGuard, SmbusError> {
        match NamedMutex::acquire(self.name, self.timeout_ms) {
            Ok(held) => Ok(Box::new(held)),
            // The wait timing out raised TimeoutError; anything else OSError.
            Err(error) if error.0.starts_with("Timed out") => Err(SmbusError::Timeout(error.0)),
            Err(error) => Err(SmbusError::Bus(error.0)),
        }
    }
}

/// `time.monotonic()`: seconds since an arbitrary fixed point.
pub fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// `time.sleep(seconds)`.
pub fn sleep_seconds(seconds: f64) {
    if seconds > 0.0 && seconds.is_finite() {
        std::thread::sleep(std::time::Duration::from_secs_f64(seconds));
    }
}

/// How a transport is built: the Python constructors' keyword arguments.
///
/// `mutex`, `monotonic` and `sleep` default to the named Windows mutex and the
/// real clock; tests pass stand-ins. `base` skips the controller discovery.
pub struct SmbusOptions {
    pub mutex: Option<Box<dyn BusMutex>>,
    pub base: Option<u16>,
    pub monotonic: Option<Box<dyn Fn() -> f64 + Send + Sync>>,
    pub sleep: Option<Box<dyn Fn(f64) + Send + Sync>>,
    pub timeout: f64,
}

impl Default for SmbusOptions {
    fn default() -> Self {
        SmbusOptions { mutex: None, base: None, monotonic: None, sleep: None, timeout: 0.05 }
    }
}

/// The platform's transport: Python's `(reader factory, controllers, hubs,
/// pmics)` tuple.
#[derive(Clone, Copy)]
pub struct SmbusBackend {
    pub factory: fn() -> Result<Arc<dyn SmbusReader>, SmbusError>,
    pub controllers: &'static [u8],
    pub hubs: &'static [u8],
    pub pmics: &'static [u8],
}

impl fmt::Debug for SmbusBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmbusBackend")
            .field("controllers", &self.controllers)
            .field("hubs", &self.hubs)
            .field("pmics", &self.pmics)
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Telemetry
// ---------------------------------------------------------------------------

// The ADC multiplexer: write the channel to R30h, wait, read the sample.
pub const TELEMETRY_SELECT_REGISTER: u8 = 0x30;
pub const TELEMETRY_VALUE_REGISTER: u8 = 0x31;

// JESD301-2 asks for 9 ms after selecting a channel. That is the floor, not
// enough here: at 9 ms with two DIMMs polled in turn, R31h still held the
// previous channel's conversion often enough to rotate the readings -- VDDQ
// showing VDD's value, VPP showing VDDQ's. A stale sample is indistinguishable
// from a real one, since it is a real voltage, just the wrong rail's.
//
// So: longer settle, and the value register is read twice with the first read
// discarded. The discard costs one SMBus transaction and closes the window
// where a conversion that finished during the first read is missed by it.
pub const TELEMETRY_SETTLE_SECONDS: f64 = 0.012;

// Channel codes, and the mV each LSB is worth (JESD301-2 Table 137). The rail
// channels and the two internal supplies step 15 mV; the bulk input, being a
// far wider range, steps 70 mV.
pub const ADC_SWA_VDD: u8 = 0x0;
pub const ADC_SWB_VDDQ: u8 = 0x2;
pub const ADC_SWC_VPP: u8 = 0x3;
pub const ADC_VIN_BULK: u8 = 0x5;
pub const ADC_VOUT_1V8: u8 = 0x8;
pub const ADC_VOUT_1V0: u8 = 0x9;

pub const ADC_STEP_MV: &[(u8, u32)] = &[
    (ADC_SWA_VDD, 15),
    (ADC_SWB_VDDQ, 15),
    (ADC_SWC_VPP, 15),
    (ADC_VOUT_1V8, 15),
    (ADC_VOUT_1V0, 15),
    (ADC_VIN_BULK, 70),
];

// Telemetry registers. SWB/SWC report in the low six bits.
pub const SWA_TELEMETRY_REGISTER: u8 = 0x0C;
pub const SWB_TELEMETRY_REGISTER: u8 = 0x0E;
pub const SWC_TELEMETRY_REGISTER: u8 = 0x0F;
pub const TELEMETRY_MODE_REGISTER: u8 = 0x1B; // bit 6: reports power rather than current
pub const TELEMETRY_TOTAL_REGISTER: u8 = 0x1A; // bit 1: SWA carries the combined total
pub const CURRENT_LIMIT_REGISTER: u8 = 0x20; // SWA [7:6], SWB [3:2], SWC [1:0]
pub const RAIL_CONFIG_REGISTER: u8 = 0x29; // bit 3: SWA runs two phases

pub const POWER_STEP_W: f64 = 0.125; // JESD301-2, power mode
pub const SWAB_CURRENT_LIMIT_MA: [u32; 4] = [3000, 4000, 5000, 6000];
pub const SWC_CURRENT_LIMIT_MA: [u32; 4] = [500, 750, 1000, 1250];

// A sample past what the channel can physically carry is a failed conversion,
// not a reading. The DIMM rails and the two internal supplies sit under 3 V;
// the bulk input is 5 V on a UDIMM and 12 V on other module types.
pub const ADC_MAX_MV: &[(u8, u32)] = &[(ADC_VIN_BULK, 15000)];
pub const ADC_DEFAULT_MAX_MV: u32 = 3000;

fn lookup(table: &[(u8, u32)], key: u8) -> Option<u32> {
    table.iter().find(|(k, _)| *k == key).map(|(_, v)| *v)
}

/// Millivolts for an ADC sample, or `None` when the channel is idle.
pub fn decode_adc_mv(channel: u8, raw: u8) -> Option<u32> {
    let step = lookup(ADC_STEP_MV, channel)?;
    if raw == 0 {
        return None;
    }
    let millivolts = raw as u32 * step;
    if millivolts > lookup(ADC_MAX_MV, channel).unwrap_or(ADC_DEFAULT_MAX_MV) {
        return None;
    }
    Some(millivolts)
}

/// The R30h value that selects an ADC channel: enable bit plus the code.
pub fn select_byte(channel: u8) -> u8 {
    0x80 | ((channel & 0x0F) << 3)
}

/// The three rails' configured current limits, in mA.
pub fn decode_current_limits(raw: u8) -> (u32, u32, u32) {
    (
        SWAB_CURRENT_LIMIT_MA[((raw >> 6) & 0x03) as usize],
        SWAB_CURRENT_LIMIT_MA[((raw >> 2) & 0x03) as usize],
        SWC_CURRENT_LIMIT_MA[(raw & 0x03) as usize],
    )
}

/// `(swa, swb, swc, total)` watts, or `None` when undecodable.
///
/// Two modes exist and the telemetry registers do not say which they are in,
/// so R1Bh bit 6 does: set means they report power directly, clear means they
/// report current as a fraction of the rail's configured limit and the rail
/// voltage turns it into watts.
///
/// `volts` is keyed `"vdd"`, `"vddq"` and `"vpp"`; a missing rail counts as
/// 0 V.
pub fn decode_power_watts(registers: &BTreeMap<u8, u8>, volts: &BTreeMap<&str, f64>) -> Option<(f64, f64, f64, f64)> {
    let swa_raw = *registers.get(&SWA_TELEMETRY_REGISTER)?;
    let swb_raw = *registers.get(&SWB_TELEMETRY_REGISTER)? & 0x3F;
    let swc_raw = *registers.get(&SWC_TELEMETRY_REGISTER)? & 0x3F;
    let mode = *registers.get(&TELEMETRY_MODE_REGISTER)?;

    if mode & (1 << 6) != 0 {
        let swa = swa_raw as f64 * POWER_STEP_W;
        let swb = swb_raw as f64 * POWER_STEP_W;
        let swc = swc_raw as f64 * POWER_STEP_W;
        let total_flag = registers.get(&TELEMETRY_TOTAL_REGISTER).copied().unwrap_or(0) & (1 << 1);
        if total_flag != 0 && swa >= swb + swc {
            // SWA carries the combined figure; the rail's own share is the
            // remainder rather than the whole.
            return Some((swa - swb - swc, swb, swc, swa));
        }
        return Some((swa, swb, swc, swa + swb + swc));
    }

    let limits = decode_current_limits(*registers.get(&CURRENT_LIMIT_REGISTER)?);
    let phases = if registers.get(&RAIL_CONFIG_REGISTER)? & (1 << 3) != 0 { 2.0 } else { 1.0 };

    let (swa_limit, swb_limit, swc_limit) = limits;
    // SWA's code is eight bits of its limit; SWB and SWC are six.
    let swa_amps = swa_raw as f64 * (swa_limit as f64 / 1000.0) / 256.0 * phases;
    let swb_amps = swb_raw as f64 * (swb_limit as f64 / 1000.0) / 64.0;
    let swc_amps = swc_raw as f64 * (swc_limit as f64 / 1000.0) / 64.0;

    let swa = swa_amps * volts.get("vdd").copied().unwrap_or(0.0);
    let swb = swb_amps * volts.get("vddq").copied().unwrap_or(0.0);
    let swc = swc_amps * volts.get("vpp").copied().unwrap_or(0.0);
    Some((swa, swb, swc, swa + swb + swc))
}

// Who made the PMIC and which revision it is. JEP106 pair in R3Ch/R3Dh, and a
// revision byte whose fields are packed rather than BCD.
//
// The revision decode confirms itself: R3Bh reads 0x12 on this bench, which
// gives major ((0x12 >> 4) & 3) + 1 = 2 and minor (0x12 >> 1) & 7 = 1, and
// ZenTimings shows "rev 2.1" for the same part.
pub const VENDOR_BANK_REGISTER: u8 = 0x3C;
pub const VENDOR_CODE_REGISTER: u8 = 0x3D;
pub const REVISION_REGISTER: u8 = 0x3B;

/// (JEP106 bank, code) -> name, entered when confirmed against a reference.
pub const PMIC_VENDOR_NAMES: &[((u32, u8), &str)] = &[
    // JEP106's "Richtek Power", shortened as the SPD tab shows the same ID.
    ((11, 0x8C), "Richtek"),
];

/// The PMIC maker's name, or its raw JEP106 ID when unlisted.
pub fn decode_pmic_vendor(bank_byte: Option<u8>, code_byte: Option<u8>) -> Option<String> {
    let (bank_byte, code_byte) = (bank_byte?, code_byte?);
    let bank = (bank_byte as u32 & 0x7F) + 1;
    let code = code_byte;
    Some(
        PMIC_VENDOR_NAMES
            .iter()
            .find(|(key, _)| *key == (bank, code))
            .map(|(_, name)| name.to_string())
            .unwrap_or_else(|| format!("0x{bank_byte:02X}{code:02X}")),
    )
}

/// The PMIC revision as "2.1", or `None` when it did not read.
pub fn decode_pmic_revision(raw: Option<u8>) -> Option<String> {
    let raw = raw?;
    Some(format!("{}.{}", ((raw >> 4) & 0x03) + 1, (raw >> 1) & 0x07))
}

/// One PMIC's maker and revision.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PmicIdentity {
    pub vendor: Option<String>,
    pub revision: Option<String>,
}

/// `{"vendor": ..., "revision": ...}` for one PMIC.
///
/// Empty when the bus does not answer; an error only for what the Python let
/// escape (a reader that cannot do byte reads at all).
pub fn read_pmic_identity(reader: &dyn SmbusReader, address: u8, controller: u8) -> Result<PmicIdentity, SmbusError> {
    let mut identity = PmicIdentity::default();
    let read = || -> Result<(u8, u8, u8), SmbusError> {
        Ok((
            reader.read_byte(address, VENDOR_BANK_REGISTER, controller)?,
            reader.read_byte(address, VENDOR_CODE_REGISTER, controller)?,
            reader.read_byte(address, REVISION_REGISTER, controller)?,
        ))
    };
    let (bank, code, revision) = match read() {
        Ok(values) => values,
        Err(error) if error.is_transfer() => return Ok(identity),
        Err(error) => return Err(error),
    };
    if let Some(vendor) = decode_pmic_vendor(Some(bank), Some(code))
        && !vendor.is_empty()
    {
        identity.vendor = Some(vendor);
    }
    if let Some(revision_text) = decode_pmic_revision(Some(revision))
        && !revision_text.is_empty()
    {
        identity.revision = Some(revision_text);
    }
    Ok(identity)
}

/// Select an ADC channel, let it settle, and return the sample in mV.
///
/// `sleep` defaults to the real one.
pub fn read_adc_millivolts(
    reader: &dyn SmbusReader,
    address: u8,
    channel: u8,
    controller: u8,
    sleep: Option<&dyn Fn(f64)>,
) -> Result<Option<u32>, SmbusError> {
    let sleep: &dyn Fn(f64) = sleep.unwrap_or(&sleep_seconds);
    let sample = || -> Result<u8, SmbusError> {
        reader.write_byte(address, TELEMETRY_SELECT_REGISTER, select_byte(channel), controller)?;
        sleep(TELEMETRY_SETTLE_SECONDS);
        // First read is discarded; see TELEMETRY_SETTLE_SECONDS.
        reader.read_byte(address, TELEMETRY_VALUE_REGISTER, controller)?;
        reader.read_byte(address, TELEMETRY_VALUE_REGISTER, controller)
    };
    match sample() {
        Ok(raw) => Ok(decode_adc_mv(channel, raw)),
        Err(error) if error.is_transfer() => Ok(None),
        Err(error) => Err(error),
    }
}

/// The PMIC that belongs to an SPD hub, by slot position.
///
/// Both devices are addressed by slot: hubs run 0x50-0x57 and PMICs 0x48-0x4F
/// in the same order, so slot n answers at 0x50+n and 0x48+n. The bench proves
/// the pairing -- hubs 0x51 and 0x53 sit with PMICs 0x49 and 0x4B, which is
/// what ZenTimings labels those two DIMMs. The Z790 bench proves it again at
/// the other slot positions: hubs 0x50 and 0x52 with PMICs 0x48 and 0x4A.
///
/// Either list left out is the Intel transport's.
pub fn pmic_address_for_hub(
    hub_address: u8,
    hub_addresses: Option<&[u8]>,
    pmic_addresses: Option<&[u8]>,
) -> Option<u8> {
    use crate::intel::intel_pch_smbus::{PMIC_ADDRESSES, SPD_HUB_ADDRESSES};

    let hub_addresses = hub_addresses.unwrap_or(&SPD_HUB_ADDRESSES);
    let pmic_addresses = pmic_addresses.unwrap_or(&PMIC_ADDRESSES);
    let first = *hub_addresses.first()?;
    let index = hub_address as i64 - first as i64;
    if 0 <= index && (index as usize) < pmic_addresses.len() {
        return Some(pmic_addresses[index as usize]);
    }
    None
}

// The resolved transport, kept rather than re-derived.
//
// detect_current_platform opens a WMI connection and runs three queries, which
// measures about 1.08 s on the Z790 bench -- longer than the telemetry window's
// own one-second poll. Resolving it per poll left that window doing more
// waiting on WMI than reading the bus, on the Tk thread, so the UI stuttered.
// The platform cannot change while the machine is running.
static BACKEND: Memo<Option<SmbusBackend>> = Memo::new();

/// `(reader factory, controllers, hubs, pmics)` for this machine.
///
/// The DDR5 devices and their decode are identical on both platforms; the
/// host controller they hang off is not. Dispatching here mirrors how
/// timings selects a timing backend, and keeps the same safety boundary:
/// only the selected platform's transport is constructed, so an Intel reader
/// is never built on AMD hardware or the other way round.
///
/// Resolved once; see `BACKEND`.
pub fn default_smbus_backend() -> Option<SmbusBackend> {
    BACKEND.get_or_init(|| {
        // The platform is resolved once for the process and the app resolves
        // it at startup, so inside the viewer this answer already exists and
        // asking WMI again would pay that second twice. Outside the viewer
        // (the research probes) this is what resolves it.
        let profile = platform_profiles::active_platform();
        match profile {
            Platform::Am5 => {
                use crate::amd::fch_smbus::{CONTROLLER_OFFSETS, PMIC_ADDRESSES, SPD_HUB_ADDRESSES, new_reader};
                Some(SmbusBackend {
                    factory: new_reader,
                    controllers: &CONTROLLER_OFFSETS,
                    hubs: &SPD_HUB_ADDRESSES,
                    pmics: &PMIC_ADDRESSES,
                })
            }
            Platform::Lga1700Ddr4 | Platform::Lga1700Ddr5 | Platform::Lga1851 => {
                use crate::intel::intel_pch_smbus::{
                    CONTROLLER_OFFSETS, PMIC_ADDRESSES, SPD_HUB_ADDRESSES, new_reader,
                };
                Some(SmbusBackend {
                    factory: new_reader,
                    controllers: &CONTROLLER_OFFSETS,
                    hubs: &SPD_HUB_ADDRESSES,
                    pmics: &PMIC_ADDRESSES,
                })
            }
            // Cached too: a machine this project does not support will not
            // become one, and re-asking WMI every poll is the cost this cache
            // exists for.
            Platform::Unsupported => None,
        }
    })
}

/// One PMIC's measured rails (mV) and power (W); anything that did not
/// convert is `None`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PmicTelemetry {
    pub vdd: Option<u32>,
    pub vddq: Option<u32>,
    pub vpp: Option<u32>,
    pub vin_bulk: Option<u32>,
    pub vout_1v8: Option<u32>,
    pub vout_1v0: Option<u32>,
    pub power_w: Option<f64>,
    pub power_swa_w: Option<f64>,
    pub power_swb_w: Option<f64>,
    pub power_swc_w: Option<f64>,
}

/// The millivolt keys, in the order they are read.
pub const MILLIVOLT_KEYS: [&str; 6] = ["vdd", "vddq", "vpp", "vin_bulk", "vout_1v8", "vout_1v0"];

impl PmicTelemetry {
    /// A reading by its Python dict key: millivolts for the rail keys, watts
    /// for the `power_*` keys.
    pub fn get(&self, key: &str) -> Option<f64> {
        match key {
            "vdd" => self.vdd.map(f64::from),
            "vddq" => self.vddq.map(f64::from),
            "vpp" => self.vpp.map(f64::from),
            "vin_bulk" => self.vin_bulk.map(f64::from),
            "vout_1v8" => self.vout_1v8.map(f64::from),
            "vout_1v0" => self.vout_1v0.map(f64::from),
            "power_w" => self.power_w,
            "power_swa_w" => self.power_swa_w,
            "power_swb_w" => self.power_swb_w,
            "power_swc_w" => self.power_swc_w,
            _ => None,
        }
    }

    fn millivolts_mut(&mut self, key: &str) -> &mut Option<u32> {
        match key {
            "vdd" => &mut self.vdd,
            "vddq" => &mut self.vddq,
            "vpp" => &mut self.vpp,
            "vin_bulk" => &mut self.vin_bulk,
            "vout_1v8" => &mut self.vout_1v8,
            _ => &mut self.vout_1v0,
        }
    }
}

/// One populated DIMM: its slot's SPD hub temperature and its own PMIC.
#[derive(Clone, Debug, PartialEq)]
pub struct DimmTelemetry {
    pub channel: &'static str,
    pub hub_address: u8,
    pub pmic_address: u8,
    pub controller: u8,
    pub hub_temp_c: Option<f64>,
    pub vendor: Option<String>,
    pub revision: Option<String>,
    pub telemetry: PmicTelemetry,
}

impl DimmTelemetry {
    /// A numeric reading by its Python dict key (`hub_temp_c`, the millivolt
    /// keys, the `power_*` keys).
    pub fn get(&self, key: &str) -> Option<f64> {
        if key == "hub_temp_c" {
            return self.hub_temp_c;
        }
        self.telemetry.get(key)
    }
}

/// One telemetry entry per populated DIMM, newest values only.
///
/// Each entry carries the slot's SPD hub temperature and its own PMIC's
/// measured rails, so two modules on one board setting report separately --
/// which is the point: their VDDQ differs here by 15 mV.
///
/// With no transport given the platform picks one; see
/// [`default_smbus_backend`]. A caller may pass its own instead, which is what
/// the tests do.
pub fn read_dimm_telemetry(
    reader_factory: Option<ReaderFactory>,
    sleep: Option<&dyn Fn(f64)>,
    controllers: Option<&[u8]>,
    addresses: Option<&[u8]>,
    pmic_addresses: Option<&[u8]>,
) -> Vec<DimmTelemetry> {
    use super::ddr5_pmic::{read_dimm_temperatures, spd_hub_channel};

    let run = || -> Result<Vec<DimmTelemetry>, SmbusError> {
        let mut reader_factory = reader_factory;
        let mut controllers = controllers;
        let mut addresses = addresses;
        let mut pmic_addresses = pmic_addresses;
        let backend;
        if reader_factory.is_none() || controllers.is_none() || addresses.is_none() {
            backend = match default_smbus_backend() {
                Some(found) => found,
                None => return Ok(Vec::new()),
            };
            if reader_factory.is_none() {
                reader_factory = Some(&backend.factory);
            }
            controllers = controllers.or(Some(backend.controllers));
            addresses = addresses.or(Some(backend.hubs));
            pmic_addresses = pmic_addresses.or(Some(backend.pmics));
        }
        let (Some(reader_factory), Some(controllers), Some(addresses)) = (reader_factory, controllers, addresses)
        else {
            return Ok(Vec::new());
        };

        let reader = reader_factory()?;
        if !reader.is_driver_open() {
            return Ok(Vec::new());
        }

        let shared = reader.clone();
        let temperatures =
            read_dimm_temperatures(Some(&move || Ok(shared.clone())), Some(controllers), Some(addresses));
        let mut modules = Vec::new();
        for &controller in controllers {
            for &hub in addresses {
                let Some(channel) = spd_hub_channel(hub) else {
                    continue;
                };
                if !reader.probe_address(hub, controller) {
                    continue;
                }
                let pmic = match pmic_address_for_hub(hub, Some(addresses), pmic_addresses) {
                    Some(pmic) if reader.probe_address(pmic, controller) => pmic,
                    _ => continue,
                };
                let identity = read_pmic_identity(reader.as_ref(), pmic, controller)?;
                let telemetry = read_pmic_telemetry(reader.as_ref(), pmic, controller, sleep)?;
                modules.push(DimmTelemetry {
                    channel,
                    hub_address: hub,
                    pmic_address: pmic,
                    controller,
                    hub_temp_c: temperatures.get(channel).copied(),
                    vendor: identity.vendor,
                    revision: identity.revision,
                    telemetry,
                });
            }
            if !modules.is_empty() {
                // DIMMs live on one controller; probing an empty address on
                // the other one blocks for nothing.
                break;
            }
        }
        Ok(modules)
    };
    run().unwrap_or_default()
}

/// One PMIC's measured rails and power.
///
/// Millivolt readings keyed by rail plus `power_w`, with anything that did
/// not convert simply absent. Never fails for a bus that will not answer: a
/// DIMM that is not there is not an error.
pub fn read_pmic_telemetry(
    reader: &dyn SmbusReader,
    address: u8,
    controller: u8,
    sleep: Option<&dyn Fn(f64)>,
) -> Result<PmicTelemetry, SmbusError> {
    use super::ddr5_pmic::{CONFIRMED_PMIC_RAILS, decode_rails};

    let mut measured = PmicTelemetry::default();
    for (name, channel) in [
        ("vdd", ADC_SWA_VDD),
        ("vddq", ADC_SWB_VDDQ),
        ("vpp", ADC_SWC_VPP),
        ("vin_bulk", ADC_VIN_BULK),
        ("vout_1v8", ADC_VOUT_1V8),
        ("vout_1v0", ADC_VOUT_1V0),
    ] {
        if let Some(millivolts) = read_adc_millivolts(reader, address, channel, controller, sleep)? {
            *measured.millivolts_mut(name) = Some(millivolts);
        }
    }

    let mut registers = BTreeMap::new();
    for register in [
        SWA_TELEMETRY_REGISTER,
        SWB_TELEMETRY_REGISTER,
        SWC_TELEMETRY_REGISTER,
        TELEMETRY_MODE_REGISTER,
        TELEMETRY_TOTAL_REGISTER,
        CURRENT_LIMIT_REGISTER,
        RAIL_CONFIG_REGISTER,
    ] {
        match reader.read_byte(address, register, controller) {
            Ok(value) => {
                registers.insert(register, value);
            }
            Err(error) if error.is_transfer() => continue,
            Err(error) => return Err(error),
        }
    }

    // Power needs a rail voltage. Prefer what the ADC just measured; fall back
    // to the VID setting, which is what the rail was asked for.
    let mut volts: BTreeMap<&str, f64> = BTreeMap::new();
    for name in ["vdd", "vddq", "vpp"] {
        if let Some(millivolts) = measured.get(name) {
            volts.insert(name, millivolts / 1000.0);
        }
    }
    if volts.len() < 3 {
        let configured =
            decode_rails(&|register| reader.read_byte(address, register, controller), Some(CONFIRMED_PMIC_RAILS))
                .unwrap_or_default();
        for (name, key) in [("vdd", "dram_vdd"), ("vddq", "dram_vddq"), ("vpp", "dram_vpp")] {
            if !volts.contains_key(name)
                && let Some(value) = configured.get(key)
            {
                volts.insert(name, *value);
            }
        }
    }

    if let Some((swa, swb, swc, total)) = decode_power_watts(&registers, &volts) {
        measured.power_w = Some(total);
        measured.power_swa_w = Some(swa);
        measured.power_swb_w = Some(swb);
        measured.power_swc_w = Some(swc);
    }
    Ok(measured)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // --- AdcDecodeTest: JESD301-2 Table 137: 15 mV per LSB, except the bulk
    // input at 70 mV.

    #[test]
    fn rail_and_internal_channels_step_15_mv() {
        assert_eq!(decode_adc_mv(ADC_SWA_VDD, 101), Some(1515));
        assert_eq!(decode_adc_mv(ADC_SWB_VDDQ, 96), Some(1440));
        assert_eq!(decode_adc_mv(ADC_SWC_VPP, 120), Some(1800));
        assert_eq!(decode_adc_mv(ADC_VOUT_1V8, 120), Some(1800));
        assert_eq!(decode_adc_mv(ADC_VOUT_1V0, 67), Some(1005));
    }

    #[test]
    fn the_bulk_input_steps_70_mv() {
        assert_eq!(decode_adc_mv(ADC_VIN_BULK, 70), Some(4900));
    }

    #[test]
    fn a_zero_sample_is_an_idle_channel_not_zero_volts() {
        assert_eq!(decode_adc_mv(ADC_SWA_VDD, 0), None);
    }

    #[test]
    fn an_unknown_channel_decodes_to_nothing() {
        assert_eq!(decode_adc_mv(0xF, 100), None);
    }

    #[test]
    fn an_implausible_sample_is_rejected() {
        // 70 mV per LSB tops out well past any DIMM rail.
        assert_eq!(decode_adc_mv(ADC_VIN_BULK, 255), None);
    }

    #[test]
    fn the_select_byte_carries_the_enable_bit_and_the_code() {
        assert_eq!(select_byte(ADC_VIN_BULK), 0x80 | (0x5 << 3));
        assert_eq!(select_byte(ADC_VOUT_1V0), 0x80 | (0x9 << 3));
    }

    // --- CurrentLimitTest

    #[test]
    fn the_bench_register_decodes_to_its_three_limits() {
        // R20h = 0xCF on the bench: SWA and SWB at 6 A, SWC at 1.25 A.
        assert_eq!(decode_current_limits(0xCF), (6000, 6000, 1250));
    }

    #[test]
    fn each_field_reads_its_own_bits() {
        assert_eq!(decode_current_limits(0x00), (3000, 3000, 500));
        assert_eq!(decode_current_limits(0x40), (4000, 3000, 500));
        assert_eq!(decode_current_limits(0x04), (3000, 4000, 500));
        assert_eq!(decode_current_limits(0x01), (3000, 3000, 750));
    }

    // --- PowerDecodeTest: two modes, and the registers do not say which;
    // R1Bh bit 6 does.

    fn bench() -> BTreeMap<u8, u8> {
        BTreeMap::from([
            (SWA_TELEMETRY_REGISTER, 3),
            (SWB_TELEMETRY_REGISTER, 1),
            (SWC_TELEMETRY_REGISTER, 0),
            (TELEMETRY_MODE_REGISTER, 0x05), // bit 6 clear -> current mode
            (TELEMETRY_TOTAL_REGISTER, 0x00),
            (CURRENT_LIMIT_REGISTER, 0xCF),
            (RAIL_CONFIG_REGISTER, 0x80), // bit 3 clear -> one phase
        ])
    }

    fn volts() -> BTreeMap<&'static str, f64> {
        BTreeMap::from([("vdd", 1.5), ("vddq", 1.44), ("vpp", 1.8)])
    }

    fn close(a: f64, b: f64, places: i32) -> bool {
        (a - b).abs() < 0.5 * 10f64.powi(-places)
    }

    #[test]
    fn current_mode_reproduces_the_bench_reading() {
        // ZenTimings showed 0.242 W as the minimum over the same window.
        let (swa, swb, swc, total) = decode_power_watts(&bench(), &volts()).unwrap();
        assert!(close(swa, 3.0 * 6.0 / 256.0 * 1.5, 6));
        assert!(close(swb, 1.0 * 6.0 / 64.0 * 1.44, 6));
        assert_eq!(swc, 0.0);
        assert!(close(total, 0.2405, 3));
    }

    #[test]
    fn dual_phase_doubles_the_swa_rail() {
        let mut registers = bench();
        registers.insert(RAIL_CONFIG_REGISTER, 0x88);
        let single = decode_power_watts(&bench(), &volts()).unwrap().0;
        let dual = decode_power_watts(&registers, &volts()).unwrap().0;
        assert!(close(dual, single * 2.0, 6));
    }

    #[test]
    fn power_mode_reads_the_registers_as_watts() {
        let mut registers = bench();
        registers.insert(TELEMETRY_MODE_REGISTER, 0x45);
        let (swa, swb, swc, total) = decode_power_watts(&registers, &volts()).unwrap();
        assert!(close(swa, 0.375, 7));
        assert!(close(swb, 0.125, 7));
        assert!(close(swc, 0.0, 7));
        assert!(close(total, 0.5, 7));
    }

    #[test]
    fn a_combined_total_is_not_counted_twice() {
        // R1Ah bit 1: SWA carries the total, so its own share is the remainder.
        let mut registers = bench();
        registers.insert(TELEMETRY_MODE_REGISTER, 0x45);
        registers.insert(TELEMETRY_TOTAL_REGISTER, 0x02);
        registers.insert(SWA_TELEMETRY_REGISTER, 8);
        let (swa, swb, swc, total) = decode_power_watts(&registers, &volts()).unwrap();
        assert!(close(total, 1.0, 7));
        assert!(close(swa, 1.0 - swb - swc, 7));
    }

    #[test]
    fn missing_registers_decode_to_nothing() {
        assert_eq!(decode_power_watts(&BTreeMap::new(), &volts()), None);
        let mut partial = bench();
        partial.remove(&CURRENT_LIMIT_REGISTER);
        assert_eq!(decode_power_watts(&partial, &volts()), None);
    }

    // --- VendorTest

    #[test]
    fn the_bench_pmic_identifies_itself() {
        // R3Ch/R3Dh 0x8A/0x8C, R3Bh 0x12 -> Richtek Power rev 2.1, which is
        // what ZenTimings shows for the same part.
        // JEP106's "Richtek Power", named as the SPD tab names it.
        assert_eq!(decode_pmic_vendor(Some(0x8A), Some(0x8C)).as_deref(), Some("Richtek"));
        assert_eq!(decode_pmic_revision(Some(0x12)).as_deref(), Some("2.1"));
    }

    #[test]
    fn an_unlisted_vendor_reports_its_raw_id() {
        assert_eq!(decode_pmic_vendor(Some(0x80), Some(0x11)).as_deref(), Some("0x8011"));
    }

    #[test]
    fn nothing_read_reports_nothing() {
        assert_eq!(decode_pmic_vendor(None, Some(0x8C)), None);
        assert_eq!(decode_pmic_revision(None), None);
    }

    /// An SMBus stand-in that records writes and answers from a register map.
    struct FakeReader {
        registers: BTreeMap<u8, u8>,
        adc: BTreeMap<u8, u8>,
        writes: Mutex<Vec<(u8, u8, u8)>>,
        selected: Mutex<Option<u8>>,
        driver_open: bool,
        // Overrides for the subclasses the Python tests made.
        samples: Option<Mutex<Vec<u8>>>,
        refuse_writes: bool,
        answering: Option<Vec<u8>>,
        probed: Mutex<Vec<(u8, u8)>>,
    }

    impl FakeReader {
        fn new(registers: BTreeMap<u8, u8>, adc: BTreeMap<u8, u8>) -> Self {
            FakeReader {
                registers,
                adc,
                writes: Mutex::new(Vec::new()),
                selected: Mutex::new(None),
                driver_open: true,
                samples: None,
                refuse_writes: false,
                answering: None,
                probed: Mutex::new(Vec::new()),
            }
        }
    }

    impl SmbusReader for FakeReader {
        fn is_driver_open(&self) -> bool {
            self.driver_open
        }

        fn write_byte(&self, address: u8, register: u8, value: u8, _controller: u8) -> Result<(), SmbusError> {
            if self.refuse_writes {
                return Err(SmbusError::Refused("not permitted".into()));
            }
            if register != TELEMETRY_SELECT_REGISTER {
                return Err(SmbusError::Refused("only the telemetry selector may be written".into()));
            }
            self.writes.lock().unwrap().push((address, register, value));
            *self.selected.lock().unwrap() = Some((value >> 3) & 0x0F);
            Ok(())
        }

        fn read_byte(&self, _address: u8, register: u8, _controller: u8) -> Result<u8, SmbusError> {
            if register == TELEMETRY_VALUE_REGISTER {
                if let Some(samples) = &self.samples {
                    let mut samples = samples.lock().unwrap();
                    return Ok(if samples.is_empty() { 96 } else { samples.remove(0) });
                }
                let selected = *self.selected.lock().unwrap();
                return Ok(selected.and_then(|s| self.adc.get(&s).copied()).unwrap_or(0));
            }
            if let Some(value) = self.registers.get(&register) {
                return Ok(*value);
            }
            Err(SmbusError::Bus("no such register".into()))
        }

        fn probe_address(&self, address: u8, controller: u8) -> bool {
            self.probed.lock().unwrap().push((controller, address));
            match &self.answering {
                Some(answering) => answering.contains(&address),
                None => false,
            }
        }
    }

    // --- ReadAdcTest

    #[test]
    fn the_channel_is_selected_then_read_back() {
        let reader = FakeReader::new(BTreeMap::new(), BTreeMap::from([(ADC_VIN_BULK, 70)]));
        let slept = Mutex::new(Vec::new());
        let millivolts =
            read_adc_millivolts(&reader, 0x49, ADC_VIN_BULK, 0x00, Some(&|s| slept.lock().unwrap().push(s))).unwrap();
        assert_eq!(millivolts, Some(4900));
        assert_eq!(*reader.writes.lock().unwrap(), vec![(0x49, 0x30, select_byte(ADC_VIN_BULK))]);
        assert_eq!(*slept.lock().unwrap(), vec![TELEMETRY_SETTLE_SECONDS]);
    }

    #[test]
    fn a_stale_sample_is_never_the_one_returned() {
        // The bug this guards: at the 9 ms the spec asks for, R31h still held
        // the previous channel's conversion often enough to rotate the
        // readings -- VDDQ showing VDD's value. A stale sample is a real
        // voltage from the wrong rail, so nothing downstream can spot it.
        let mut reader = FakeReader::new(BTreeMap::new(), BTreeMap::new());
        reader.samples = Some(Mutex::new(vec![100, 96])); // stale, then the real one
        let millivolts = read_adc_millivolts(&reader, 0x49, ADC_SWB_VDDQ, 0x00, Some(&|_| {})).unwrap();
        assert_eq!(millivolts, Some(96 * 15));
    }

    #[test]
    fn the_settle_clears_the_spec_floor() {
        const { assert!(TELEMETRY_SETTLE_SECONDS >= 0.009) };
    }

    #[test]
    fn a_refused_write_reports_no_reading() {
        let mut reader = FakeReader::new(BTreeMap::new(), BTreeMap::new());
        reader.refuse_writes = true;
        assert_eq!(read_adc_millivolts(&reader, 0x49, ADC_VIN_BULK, 0x00, Some(&|_| {})).unwrap(), None);
    }

    #[test]
    fn only_the_selector_is_ever_written() {
        let reader = FakeReader::new(bench(), BTreeMap::from([(ADC_SWA_VDD, 100)]));
        read_pmic_telemetry(&reader, 0x49, 0x00, Some(&|_| {})).unwrap();
        let writes = reader.writes.lock().unwrap();
        assert!(!writes.is_empty());
        for (_address, register, _value) in writes.iter() {
            assert_eq!(*register, TELEMETRY_SELECT_REGISTER);
        }
    }

    // --- BackendCacheTest: resolving the transport must not cost a WMI round
    // trip per poll.

    #[test]
    fn the_platform_is_asked_once() {
        // detect_current_platform opens a WMI connection and runs three
        // queries -- about a second on the bench, against a one-second poll.
        // Ported as: once resolved, a different platform answer is never
        // consulted again.
        platform_profiles::set_active_platform(platform_profiles::UNSUPPORTED);
        for _ in 0..5 {
            assert!(default_smbus_backend().is_none());
        }
        platform_profiles::set_active_platform(platform_profiles::AM5);
        assert!(default_smbus_backend().is_none());
    }

    #[test]
    fn an_unsupported_machine_is_cached_too() {
        // Otherwise the one case with nothing to show pays the most for it.
        platform_profiles::set_active_platform(platform_profiles::UNSUPPORTED);
        assert!(default_smbus_backend().is_none());
        assert_eq!(BACKEND.get().map(|b| b.is_none()), Some(true));
        assert!(default_smbus_backend().is_none());
    }

    #[test]
    fn a_supported_platform_names_its_own_transport() {
        platform_profiles::set_active_platform(platform_profiles::AM5);
        let backend = default_smbus_backend().unwrap();
        assert_eq!(backend.controllers, &[0x00, 0x20]);
        BACKEND.clear();
        platform_profiles::set_active_platform(platform_profiles::LGA1700_DDR5);
        let backend = default_smbus_backend().unwrap();
        assert_eq!(backend.controllers, &[0x00]);
        assert_eq!(backend.hubs, &[0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57]);
        assert_eq!(backend.pmics, &[0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F]);
    }

    // --- IntelBusTest: the DDR5 devices decode the same on either host
    // controller.

    const HUBS: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];
    const PMICS: [u8; 8] = [0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F];

    #[test]
    fn the_pairing_holds_on_the_intel_address_lists() {
        // The Z790 bench answers at the first slot of each channel, where the
        // AM5 bench answers at the second. Same slot-position rule either way.
        assert_eq!(pmic_address_for_hub(0x50, Some(&HUBS), Some(&PMICS)), Some(0x48));
        assert_eq!(pmic_address_for_hub(0x52, Some(&HUBS), Some(&PMICS)), Some(0x4A));
    }

    #[test]
    fn a_hub_past_the_list_pairs_with_nothing() {
        assert_eq!(pmic_address_for_hub(0x60, Some(&HUBS), Some(&PMICS)), None);
    }

    #[test]
    fn one_controller_is_walked_when_the_platform_has_one() {
        // The PCH exposes a single controller. Passing (0x00,) must not send
        // the scan looking for a second one that does not exist.
        let reader = Arc::new(FakeReader::new(BTreeMap::new(), BTreeMap::new()));
        let shared = reader.clone();
        read_dimm_telemetry(
            Some(&move || Ok(shared.clone() as Arc<dyn SmbusReader>)),
            None,
            Some(&[0x00]),
            Some(&HUBS),
            Some(&PMICS),
        );
        let seen = reader.probed.lock().unwrap();
        assert!(!seen.is_empty());
        assert!(seen.iter().all(|(controller, _)| *controller == 0x00));
    }

    #[test]
    fn a_populated_slot_reports_its_own_pmic() {
        let registers = BTreeMap::from([
            (SWA_TELEMETRY_REGISTER, 3),
            (SWB_TELEMETRY_REGISTER, 4),
            (SWC_TELEMETRY_REGISTER, 0),
            (TELEMETRY_MODE_REGISTER, 0x05),
            (TELEMETRY_TOTAL_REGISTER, 0x00),
            (CURRENT_LIMIT_REGISTER, 0xCF),
            (RAIL_CONFIG_REGISTER, 0xB0),
        ]);
        let mut reader = FakeReader::new(registers, BTreeMap::new());
        reader.answering = Some(vec![0x50, 0x48]);
        let reader = Arc::new(reader);
        let entries = read_dimm_telemetry(
            Some(&move || Ok(reader.clone() as Arc<dyn SmbusReader>)),
            Some(&|_| {}),
            Some(&[0x00]),
            Some(&HUBS),
            Some(&PMICS),
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].channel, "a");
        assert_eq!(entries[0].hub_address, 0x50);
        assert_eq!(entries[0].pmic_address, 0x48);
    }

    // --- PmicPairingTest

    #[test]
    fn a_hub_pairs_with_the_pmic_in_the_same_slot() {
        // The bench: hubs 0x51 and 0x53 with PMICs 0x49 and 0x4B.
        assert_eq!(pmic_address_for_hub(0x51, None, None), Some(0x49));
        assert_eq!(pmic_address_for_hub(0x53, None, None), Some(0x4B));
        assert_eq!(pmic_address_for_hub(0x50, None, None), Some(0x48));
    }

    #[test]
    fn an_address_outside_the_hub_range_pairs_with_nothing() {
        assert_eq!(pmic_address_for_hub(0x60, None, None), None);
    }
}
