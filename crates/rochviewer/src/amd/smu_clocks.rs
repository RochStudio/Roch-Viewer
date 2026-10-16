// Roch Viewer -- a read-only memory-controller and timing viewer.
// Copyright (C) 2026 Roch Studio
//
// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Fail-closed AM5 FCLK/UCLK reader via RSMU PM-table telemetry.
//!
//! On Ryzen 9000 only the exact validated table version `0x620105` is
//! decoded, using the CONFIRMED offsets from the MIT gnr-smu map:
//!
//! ```text
//!   FCLK @ 0x11C (float MHz)
//!   UCLK @ 0x12C (float MHz)
//!   MCLK @ 0x13C (float MHz, cross-check only)
//! ```
//!
//! Ryzen 7000 is read experimentally from ZenStates-Core's Zen 4 table list
//! ([`RAPHAEL_CLOCK_TABLES`]); its results carry `verified == false`.
//!
//! Flow (fixed addresses / commands only):
//!   1. CPU gate (desktop Ryzen 9000 Granite Ridge, or Ryzen 7000 Raphael)
//!   2. RSMU cmd 0x05 -> table version must be one listed for that CPU
//!   3. RSMU cmd 0x04 args [1,1,0,0,0,0] -> firmware DRAM base
//!   4. RSMU cmd 0x03 args all-zero -> transfer table into that base
//!   5. Read physical floats at the fixed offsets
//!   6. Validate ranges; optionally cross-check UCLK vs UMC MCLK

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::hardware::pci_mcfg::{ecam_address, get_mcfg_table, parse_mcfg, select_allocation};
use crate::hardware::{HwError, HwResult, hw};
use crate::memo::Memo;
use crate::platform_profiles::{is_granite_ridge_cpu, is_raphael_cpu};
use crate::pyfmt::round_half_even;

use super::smn::{
    AMD_VENDOR_ID, DEFAULT_MUTEX_NAME, NamedPciMutex, PciMutex, RSMU_ARG0, RSMU_MSG, RSMU_RSP,
    RSMU_TABLE_VERSION_COMMAND, SMN_DATA_REG, SMN_INDEX_REG, VENDOR_REG,
};

pub const RSMU_TABLE_ADDRESS_COMMAND: u32 = 0x04;
pub const RSMU_TABLE_TRANSFER_COMMAND: u32 = 0x03;

// GetPBOScalar. The fourth and last permitted command, and the only one that
// is not part of the PM-table sequence.
//
// Provenance: ZenStates-Core's Zen4Settings, which Zen5Settings inherits
// unchanged for the RSMU mailbox. The same class defines this project's other
// three commands and all three mailbox addresses, and those are already
// confirmed working on this hardware, so the message block is the validated
// one rather than a number found somewhere and hoped for. It is a getter: it
// takes no arguments and returns the scalar in arg0.
pub const RSMU_PBO_SCALAR_COMMAND: u32 = 0x6D;

// Every command this project may issue. Each is a read; nothing here sets a
// limit, a voltage or a frequency, and no write message is reachable at all.
pub const PERMITTED_COMMANDS: [u32; 4] = [
    RSMU_TABLE_VERSION_COMMAND,
    RSMU_TABLE_ADDRESS_COMMAND,
    RSMU_TABLE_TRANSFER_COMMAND,
    RSMU_PBO_SCALAR_COMMAND,
];
pub const EXPECTED_TABLE_VERSION: u32 = 0x620105;
pub const PM_TABLE_LENGTH: u64 = 0x724;
pub const ADDRESS_REQUEST_ARGUMENTS: [u32; 6] = [1, 1, 0, 0, 0, 0];
const ZERO_ARGUMENTS: [u32; 6] = [0, 0, 0, 0, 0, 0];

pub const OFFSET_FCLK: u64 = 0x11C;
pub const OFFSET_UCLK: u64 = 0x12C;
pub const OFFSET_MCLK: u64 = 0x13C;

/// Where one PM-table version keeps its three clocks, and how long it is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClockTableLayout {
    pub length: u64,
    pub fclk: u64,
    pub uclk: u64,
    pub mclk: u64,
    /// True only for a version whose offsets this project has confirmed on
    /// real hardware. The rest come from ZenStates-Core's tables, which mark
    /// their Zen 4 entries "unverified" themselves.
    pub verified: bool,
}

/// PM-table version -> layout, the Python dicts of that shape.
pub type ClockTables = [(u32, ClockTableLayout)];

/// The layout for `version` in `tables`.
pub fn clock_layout(tables: &ClockTables, version: u32) -> Option<ClockTableLayout> {
    tables.iter().find(|(known, _)| *known == version).map(|(_, layout)| *layout)
}

pub static GRANITE_RIDGE_CLOCK_TABLES: &ClockTables = &[(
    EXPECTED_TABLE_VERSION,
    ClockTableLayout { length: PM_TABLE_LENGTH, fclk: OFFSET_FCLK, uclk: OFFSET_UCLK, mclk: OFFSET_MCLK, verified: true },
)];

// Desktop Ryzen 7000 (Raphael), experimental. Versions, sizes and offsets are
// ZenStates-Core's PowerTable.cs Zen 4 entries as of 8979d27 (2026-09-24),
// whose own comment reads "offsets are not verified yet". A wrong offset here
// decodes as a float that validate_clocks then has to reject: it must be a
// whole number of MHz inside the clock ranges, and UCLK must equal the UMC's
// own MCLK or half of it. That cross-check against a register read another
// way is what makes showing these at all defensible.
const RAPHAEL_LOW: (u64, u64, u64) = (0x118, 0x128, 0x138);
const RAPHAEL_HIGH: (u64, u64, u64) = (0x11C, 0x12C, 0x13C);

const fn raphael(version: u32, length: u64, offsets: (u64, u64, u64)) -> (u32, ClockTableLayout) {
    (version, ClockTableLayout { length, fclk: offsets.0, uclk: offsets.1, mclk: offsets.2, verified: false })
}

pub static RAPHAEL_CLOCK_TABLES: &ClockTables = &[
    raphael(0x540000, 0x828, RAPHAEL_LOW), raphael(0x540001, 0x82C, RAPHAEL_LOW),
    raphael(0x540002, 0x87C, RAPHAEL_LOW), raphael(0x540003, 0x89C, RAPHAEL_LOW),
    raphael(0x540004, 0x8BC, RAPHAEL_LOW), raphael(0x540005, 0x8C8, RAPHAEL_LOW),
    raphael(0x540100, 0x618, RAPHAEL_LOW), raphael(0x540101, 0x61C, RAPHAEL_LOW),
    raphael(0x540102, 0x66C, RAPHAEL_LOW), raphael(0x540103, 0x68C, RAPHAEL_LOW),
    raphael(0x540104, 0x6A8, RAPHAEL_LOW), raphael(0x540105, 0x6B4, RAPHAEL_LOW),
    raphael(0x540108, 0x6BC, RAPHAEL_LOW),
    raphael(0x540208, 0x8D0, RAPHAEL_HIGH),
];

static NO_CLOCK_TABLES: &ClockTables = &[];

pub const FCLK_MIN_MHZ: f64 = 600.0;
pub const FCLK_MAX_MHZ: f64 = 3000.0;
pub const UCLK_MIN_MHZ: f64 = 600.0;
pub const UCLK_MAX_MHZ: f64 = 4200.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SmuClocks {
    pub version: u32,
    pub table_base: u64,
    pub fclk_mhz: f64,
    pub uclk_mhz: f64,
    pub mclk_mhz: f64,
    pub verified: bool,
}

/// "" if this CPU may be read, else the reason it may not.
///
/// The name is the whole gate. It also took family, model and core count,
/// each checked only when supplied and never supplied by anything -- so the
/// CPUID they needed was a stub returning None, and the gate that read as
/// "family 0x1A, model 0x44" was in fact the name test alone. Better one
/// check that runs than four that describe an intention.
///
/// This gate is Ryzen 9000's alone and stays so for voltages and power;
/// the clocks have their own, [`clock_tables_for_cpu`], which also admits
/// Ryzen 7000.
pub fn check_cpu_gate(cpu_name: &str) -> String {
    if !is_granite_ridge_cpu(cpu_name) {
        return "CPU is not a validated desktop Ryzen 9000 Granite Ridge part".into();
    }
    String::new()
}

/// The PM-table versions whose clocks may be read on this CPU.
///
/// Empty for anything but desktop Ryzen 9000 and 7000, and each CPU accepts
/// only its own versions: a Ryzen 7000 reporting a Ryzen 9000 table, or the
/// other way round, is refused rather than decoded.
pub fn clock_tables_for_cpu(cpu_name: &str) -> &'static ClockTables {
    if is_granite_ridge_cpu(cpu_name) {
        return GRANITE_RIDGE_CLOCK_TABLES;
    }
    if is_raphael_cpu(cpu_name) {
        return RAPHAEL_CLOCK_TABLES;
    }
    NO_CLOCK_TABLES
}

/// Decode one PM-table dword as a little-endian float.
///
/// Every consumer of this table -- clocks, voltages, power, and the research
/// probes -- reads the same 32-bit float format, so they share this decoder.
/// `finite_only == false` is for the probes, which dump arbitrary dwords and
/// must render a non-finite one rather than abort the report.
pub fn decode_table_float(raw_dword: u32, what: &str, finite_only: bool) -> Result<f64, String> {
    let value = f32::from_bits(raw_dword) as f64;
    if finite_only && !value.is_finite() {
        return Err(format!("non-finite {what} float"));
    }
    Ok(value)
}

pub fn decode_clock_float(raw_dword: u32) -> Result<f64, String> {
    decode_table_float(raw_dword, "clock", true)
}

/// `(fclk, uclk)`, or why the values look wrong.
pub fn validate_clocks(fclk: f64, uclk: f64, mclk: Option<f64>, umc_mclk: Option<f64>) -> Result<(f64, f64), String> {
    if !(FCLK_MIN_MHZ..=FCLK_MAX_MHZ).contains(&fclk) {
        return Err(format!("FCLK {fclk:.1} MHz out of range"));
    }
    if !(UCLK_MIN_MHZ..=UCLK_MAX_MHZ).contains(&uclk) {
        return Err(format!("UCLK {uclk:.1} MHz out of range"));
    }
    // Prefer near-integer MHz values (telemetry is float but clocks are discrete).
    if (fclk - round_half_even(fclk, 0)).abs() > 1.5 {
        return Err(format!("FCLK {fclk:.3} does not look like a fabric clock"));
    }
    if (uclk - round_half_even(uclk, 0)).abs() > 1.5 {
        return Err(format!("UCLK {uclk:.3} does not look like a memory clock"));
    }
    let reference = if umc_mclk.is_some() { umc_mclk } else { mclk };
    if let Some(reference) = reference
        && reference > 0.0
    {
        // UCLK is typically MCLK or MCLK/2.
        if (uclk - reference).abs().min((uclk - reference / 2.0).abs()) > 50.0f64.max(reference * 0.08) {
            return Err(format!("UCLK {uclk:.0} does not match UMC/PM MCLK {reference:.0} (or half)"));
        }
    }
    Ok((round_half_even(fclk, 0), round_half_even(uclk, 0)))
}

/// The RSMU mailbox as the PM-table readers drive it.
///
/// The real one is [`InpOutRsmuClockAccess`]; tests supply their own. Every
/// method is a fixed-function step: none takes an arbitrary physical
/// destination to write.
pub trait RsmuAccess: Send + Sync {
    fn is_driver_open(&self) -> bool;
    fn read_vendor(&self) -> HwResult<u32>;
    fn capture_selector(&self) -> HwResult<()>;
    fn selector_captured(&self) -> bool;
    fn read_response(&self) -> HwResult<u32>;
    fn clear_response(&self) -> HwResult<()>;
    fn write_arguments(&self, values: &[u32]) -> HwResult<()>;
    fn issue_command(&self, command: u32) -> HwResult<()>;
    fn read_arg0(&self) -> HwResult<u32>;
    fn read_arg1(&self) -> HwResult<u32>;
    fn restore_selector(&self) -> HwResult<()>;
    fn read_phys_dword(&self, address: u64) -> HwResult<u32>;
}

/// Fixed-function ECAM mailbox access for version/address/transfer/read.
pub struct InpOutRsmuClockAccess {
    pub config_base: u64,
    selector_address: u64,
    data_address: u64,
    prior_selector: Mutex<Option<u32>>,
    last_written_selector: Mutex<Option<u32>>,
}

impl InpOutRsmuClockAccess {
    /// The firmware's MCFG table, segment 0, bus 0.
    pub fn new() -> Result<Self, String> {
        if let Some(error) = crate::hardware::driver_error() {
            return Err(error);
        }
        Self::from_table(&get_mcfg_table()?, 0, 0)
    }

    pub fn from_table(table: &[u8], segment: u16, bus: u8) -> Result<Self, String> {
        let entries = parse_mcfg(table)?;
        let allocation = select_allocation(&entries, segment, bus)?;
        let config_base = ecam_address(&allocation, bus, 0, 0, 0)?;
        Ok(InpOutRsmuClockAccess {
            config_base,
            selector_address: config_base + SMN_INDEX_REG as u64,
            data_address: config_base + SMN_DATA_REG as u64,
            prior_selector: Mutex::new(None),
            last_written_selector: Mutex::new(None),
        })
    }

    /// The selector value most recently written, if any.
    pub fn last_written_selector(&self) -> Option<u32> {
        *self.last_written_selector.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn write_selector(&self, value: u32) -> HwResult<()> {
        let selector_address = self.selector_address;
        hw().set_phys_long(selector_address, value)
            .map_err(|_| HwError(format!("SetPhysLong failed at selector 0x{selector_address:016X}")))?;
        *self.last_written_selector.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        Ok(())
    }

    fn write_data(&self, value: u32) -> HwResult<()> {
        let data_address = self.data_address;
        hw().set_phys_long(data_address, value)
            .map_err(|_| HwError(format!("SetPhysLong failed at data window 0x{data_address:016X}")))
    }

    fn read_fixed_smn(&self, address: u32) -> HwResult<u32> {
        self.write_selector(address)?;
        hw().get_phys_long(self.data_address)
    }

    fn write_fixed_smn(&self, address: u32, value: u32) -> HwResult<()> {
        self.write_selector(address)?;
        self.write_data(value)
    }

    fn read_arg(&self, index: u32) -> HwResult<u32> {
        self.read_fixed_smn(RSMU_ARG0 + index * 4)
    }
}

impl RsmuAccess for InpOutRsmuClockAccess {
    fn is_driver_open(&self) -> bool {
        hw().is_driver_open()
    }

    fn read_vendor(&self) -> HwResult<u32> {
        Ok(hw().get_phys_long(self.config_base + VENDOR_REG as u64)? & 0xFFFF)
    }

    fn capture_selector(&self) -> HwResult<()> {
        let value = hw().get_phys_long(self.selector_address)?;
        *self.prior_selector.lock().unwrap_or_else(|e| e.into_inner()) = Some(value);
        Ok(())
    }

    fn selector_captured(&self) -> bool {
        self.prior_selector.lock().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn read_response(&self) -> HwResult<u32> {
        self.read_fixed_smn(RSMU_RSP)
    }

    fn clear_response(&self) -> HwResult<()> {
        self.write_fixed_smn(RSMU_RSP, 0)
    }

    fn write_arguments(&self, values: &[u32]) -> HwResult<()> {
        if values.len() != 6 {
            return Err(HwError::new("RSMU argument vector must have 6 DWORDs"));
        }
        for (index, value) in values.iter().enumerate() {
            self.write_fixed_smn(RSMU_ARG0 + index as u32 * 4, *value)?;
        }
        Ok(())
    }

    fn issue_command(&self, command: u32) -> HwResult<()> {
        let command = command & 0xFF;
        if !PERMITTED_COMMANDS.contains(&command) {
            return Err(HwError(format!("command 0x{command:02X} is not permitted")));
        }
        self.write_fixed_smn(RSMU_MSG, command)
    }

    fn read_arg0(&self) -> HwResult<u32> {
        self.read_arg(0)
    }

    fn read_arg1(&self) -> HwResult<u32> {
        self.read_arg(1)
    }

    fn restore_selector(&self) -> HwResult<()> {
        let prior = *self.prior_selector.lock().unwrap_or_else(|e| e.into_inner());
        let Some(prior) = prior else {
            return Err(HwError::new("No prior SMN selector was captured"));
        };
        self.write_selector(prior)
    }

    fn read_phys_dword(&self, address: u64) -> HwResult<u32> {
        hw().get_phys_long(address)
    }
}

/// Seconds on a monotonic clock: Python's `time.monotonic`.
pub fn monotonic() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// Python's `time.sleep`.
pub fn sleep(seconds: f64) {
    if seconds > 0.0 && seconds.is_finite() {
        std::thread::sleep(Duration::from_secs_f64(seconds));
    }
}

pub type MonotonicFn = Box<dyn Fn() -> f64 + Send + Sync>;
pub type SleepFn = Box<dyn Fn(f64) + Send + Sync>;

/// The Python constructor's keyword arguments, with its defaults.
pub struct RsmuOptions {
    /// Serialises with other processes; `None` is the named mutex
    /// `mutex_name`.
    pub mutex: Option<Box<dyn PciMutex>>,
    pub mutex_name: String,
    pub monotonic: MonotonicFn,
    pub sleep: SleepFn,
    pub timeout: f64,
    pub total_timeout: f64,
    pub umc_mclk_mhz: Option<f64>,
    /// `None` is [`GRANITE_RIDGE_CLOCK_TABLES`].
    pub tables: Option<&'static ClockTables>,
}

impl Default for RsmuOptions {
    fn default() -> Self {
        RsmuOptions {
            mutex: None,
            mutex_name: DEFAULT_MUTEX_NAME.into(),
            monotonic: Box::new(monotonic),
            sleep: Box::new(sleep),
            timeout: 0.25,
            total_timeout: 1.0,
            umc_mclk_mhz: None,
            tables: None,
        }
    }
}

/// Read FCLK/UCLK from the approved PM-table version only.
pub struct RsmuClockReader {
    tables: &'static ClockTables,
    pub(crate) access: Arc<dyn RsmuAccess>,
    mutex: Box<dyn PciMutex>,
    lock: Mutex<()>,
    monotonic: MonotonicFn,
    sleep: SleepFn,
    timeout: f64,
    total_timeout: f64,
    umc_mclk_mhz: Option<f64>,
    last_monotonic: Mutex<Option<f64>>,
    run_deadline: Mutex<Option<f64>>,
    last_error: Mutex<String>,
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

impl RsmuClockReader {
    /// A reader with every default: the named mutex, 0.25 s per response,
    /// 1.0 s per run, the Ryzen 9000 table.
    pub fn new(access: Arc<dyn RsmuAccess>) -> Self {
        Self::with_options(access, RsmuOptions::default()).expect("the default timeouts are valid")
    }

    pub fn with_options(access: Arc<dyn RsmuAccess>, options: RsmuOptions) -> Result<Self, String> {
        let timeout = options.timeout;
        let total_timeout = options.total_timeout;
        if !timeout.is_finite() || !(0.0 < timeout && timeout <= 0.25) {
            return Err("RSMU timeout must be finite and within (0, 0.25]".into());
        }
        if !total_timeout.is_finite() || !(0.0 < total_timeout && total_timeout <= 1.0) {
            return Err("RSMU total timeout must be finite and within (0, 1.0]".into());
        }
        let mutex = options.mutex.unwrap_or_else(|| Box::new(NamedPciMutex::new(&options.mutex_name)));
        Ok(RsmuClockReader {
            tables: options.tables.unwrap_or(GRANITE_RIDGE_CLOCK_TABLES),
            access,
            mutex,
            lock: Mutex::new(()),
            monotonic: options.monotonic,
            sleep: options.sleep,
            timeout,
            total_timeout,
            umc_mclk_mhz: options.umc_mclk_mhz,
            last_monotonic: Mutex::new(None),
            run_deadline: Mutex::new(None),
            last_error: Mutex::new(String::new()),
        })
    }

    /// Why the last read returned `None`; empty after a success.
    pub fn last_error(&self) -> String {
        locked(&self.last_error).clone()
    }

    pub(crate) fn set_last_error(&self, message: String) {
        *locked(&self.last_error) = message;
    }

    /// Forget the previous run's clock readings and deadline.
    pub(crate) fn reset_run(&self) {
        *locked(&self.last_monotonic) = None;
        *locked(&self.run_deadline) = None;
    }

    /// Arm the total-run budget from now.
    pub(crate) fn arm_run_deadline(&self) -> HwResult<()> {
        let deadline = self.clock_now()? + self.total_timeout;
        *locked(&self.run_deadline) = Some(deadline);
        Ok(())
    }

    /// Take the process lock, then the cross-process mutex, in that order.
    pub(crate) fn serialize(&self) -> HwResult<(std::sync::MutexGuard<'_, ()>, super::smn::MutexGuard)> {
        let lock = locked(&self.lock);
        let mutex = self.mutex.acquire()?;
        Ok((lock, mutex))
    }

    pub(crate) fn clock_now(&self) -> HwResult<f64> {
        let now = (self.monotonic)();
        if !now.is_finite() {
            return Err(HwError::new("RSMU monotonic clock is invalid"));
        }
        let mut last = locked(&self.last_monotonic);
        if let Some(previous) = *last
            && now < previous
        {
            return Err(HwError::new("RSMU monotonic clock moved backward"));
        }
        *last = Some(now);
        drop(last);
        if let Some(deadline) = *locked(&self.run_deadline)
            && now > deadline
        {
            return Err(HwError::new("RSMU total-run budget expired"));
        }
        Ok(now)
    }

    fn wait_for_response(&self) -> HwResult<u32> {
        let started = self.clock_now()?;
        let deadline = started + self.timeout;
        loop {
            let before = self.clock_now()?;
            if before > deadline {
                return Err(HwError::new("RSMU mailbox response timed out"));
            }
            let response = self.access.read_response()?;
            let after = self.clock_now()?;
            if after > deadline {
                return Err(HwError::new("RSMU mailbox response timed out"));
            }
            if response != 0 {
                return Ok(response);
            }
            if after >= deadline {
                return Err(HwError::new("RSMU mailbox response timed out"));
            }
            (self.sleep)(0.001f64.min(deadline - after));
        }
    }

    pub(crate) fn command(&self, command: u32, arguments: &[u32; 6]) -> HwResult<()> {
        self.wait_for_response()?;
        self.access.clear_response()?;
        self.access.write_arguments(arguments)?;
        self.access.issue_command(command)?;
        let response = self.wait_for_response()?;
        if response != 1 {
            return Err(HwError(format!("RSMU command 0x{command:02X} returned 0x{response:08X}")));
        }
        Ok(())
    }

    /// Vendor check and selector capture, then `run`, then the guaranteed
    /// selector restore: the privileged frame every PM-table read shares.
    ///
    /// A failed restore replaces whatever `run` returned, with the CRITICAL
    /// message in `last_error`, as the Python's raise inside `finally` did.
    pub(crate) fn in_mailbox_session<T>(&self, run: impl FnOnce() -> HwResult<T>) -> HwResult<T> {
        if !self.access.is_driver_open() {
            return Err(HwError::new("InpOut driver is not open"));
        }
        let _serialized = self.serialize()?;
        let vendor = self.access.read_vendor()? & 0xFFFF;
        if vendor != AMD_VENDOR_ID {
            return Err(HwError(format!("MCFG PCI 00:00.0 vendor is 0x{vendor:04X}, expected AMD 0x1022")));
        }
        self.access.capture_selector()?;
        if !self.access.selector_captured() {
            return Err(HwError::new("prior SMN selector was not captured"));
        }
        let result = run();
        if let Err(error) = self.access.restore_selector() {
            self.set_last_error(format!("CRITICAL: SMN selector restore failed ({error})"));
            return Err(error);
        }
        result
    }

    fn clock_run(&self) -> HwResult<SmuClocks> {
        self.arm_run_deadline()?;
        self.command(RSMU_TABLE_VERSION_COMMAND, &ZERO_ARGUMENTS)?;
        let version = self.access.read_arg0()?;
        let Some(layout) = clock_layout(self.tables, version) else {
            let mut known: Vec<u32> = self.tables.iter().map(|(known, _)| *known).collect();
            known.sort_unstable();
            let known: Vec<String> = known.iter().map(|known| format!("0x{known:06X}")).collect();
            let listed = if known.is_empty() { "none".to_string() } else { known.join(", ") };
            return Err(HwError(format!(
                "PM-table version 0x{version:06X} is not an approved version for this CPU ({listed})"
            )));
        };
        self.command(RSMU_TABLE_ADDRESS_COMMAND, &ADDRESS_REQUEST_ARGUMENTS)?;
        let arg0 = self.access.read_arg0()? as u64;
        let arg1 = self.access.read_arg1()? as u64;
        let base = arg0 | (arg1 << 32);
        if base == 0 || base == u64::MAX {
            return Err(HwError::new("firmware returned an invalid PM-table base"));
        }
        if base > (1 << 48) {
            return Err(HwError::new("PM-table base exceeds phys address width"));
        }
        if base + layout.length > (1 << 48) {
            return Err(HwError::new("PM-table range is not mappable"));
        }
        self.command(RSMU_TABLE_TRANSFER_COMMAND, &ZERO_ARGUMENTS)?;
        let fclk = decode_clock_float(self.access.read_phys_dword(base + layout.fclk)?).map_err(HwError)?;
        let uclk = decode_clock_float(self.access.read_phys_dword(base + layout.uclk)?).map_err(HwError)?;
        let mclk = decode_clock_float(self.access.read_phys_dword(base + layout.mclk)?).map_err(HwError)?;
        // The cross-check against the UMC's own MCLK is optional for the
        // confirmed table and the only real safeguard for an unconfirmed one,
        // so without it nothing is shown.
        if !layout.verified && self.umc_mclk_mhz.is_none() {
            return Err(HwError(format!(
                "PM-table 0x{version:06X} is unverified and the UMC MCLK it must be checked against is unavailable"
            )));
        }
        let (fclk, uclk) = validate_clocks(fclk, uclk, Some(mclk), self.umc_mclk_mhz).map_err(HwError)?;
        Ok(SmuClocks {
            version,
            table_base: base,
            fclk_mhz: fclk,
            uclk_mhz: uclk,
            mclk_mhz: if mclk.is_finite() { round_half_even(mclk, 0) } else { 0.0 },
            verified: layout.verified,
        })
    }

    pub fn read_clocks(&self) -> Option<SmuClocks> {
        self.set_last_error(String::new());
        self.reset_run();
        match self.in_mailbox_session(|| self.clock_run()) {
            Ok(clocks) => Some(clocks),
            Err(error) => {
                if self.last_error().is_empty() {
                    self.set_last_error(format!("RSMU clock read failed: {error}"));
                }
                None
            }
        }
    }
}

// Constructing this re-parses the ACPI MCFG table (two GetSystemFirmwareTable
// calls). None of that can change while the machine is running, and the
// clock, voltage and power readers each want one per read, so it is built
// once and shared.
static SHARED_ACCESS: Memo<Arc<InpOutRsmuClockAccess>> = Memo::new();

/// The process-wide RSMU access object, built on first use.
pub fn shared_rsmu_access() -> Result<Arc<InpOutRsmuClockAccess>, String> {
    if let Some(access) = SHARED_ACCESS.get() {
        return Ok(access);
    }
    let access = Arc::new(InpOutRsmuClockAccess::new()?);
    SHARED_ACCESS.set(access.clone());
    Ok(access)
}

/// Convenience entry used by Am5Runtime: `None` when the CPU has no clock
/// tables or the read fails.
pub fn read_smu_clocks(cpu_name: &str, umc_mclk_mhz: Option<f64>) -> Option<SmuClocks> {
    let tables = clock_tables_for_cpu(cpu_name);
    if tables.is_empty() {
        return None;
    }
    let access = match shared_rsmu_access() {
        Ok(access) => access,
        Err(_) => {
            SHARED_ACCESS.clear(); // rebuild next time
            return None;
        }
    };
    let options = RsmuOptions { umc_mclk_mhz, tables: Some(tables), ..RsmuOptions::default() };
    match RsmuClockReader::with_options(access, options) {
        Ok(reader) => reader.read_clocks(),
        Err(_) => {
            SHARED_ACCESS.clear(); // rebuild next time
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::amd::smn::tests::FakeMutex;
    use std::sync::atomic::{AtomicU32, Ordering};

    pub(crate) fn as_dword(value: f32) -> u32 {
        value.to_bits()
    }

    // -- SmuClockHelpersTest (test_am5_clocks_agesa.py) ----------------------

    #[test]
    fn cpu_gate_accepts_9850x3d() {
        assert_eq!(check_cpu_gate("AMD Ryzen 7 9850X3D 8-Core Processor"), "");
    }

    #[test]
    fn cpu_gate_rejects_intel() {
        assert!(!check_cpu_gate("Intel(R) Core(TM) Ultra 7 270K Plus").is_empty());
    }

    #[test]
    fn decode_and_validate_zen_timings_shape() {
        let fclk = decode_clock_float(as_dword(2000.0)).unwrap();
        let uclk = decode_clock_float(as_dword(2050.0)).unwrap();
        let (f, u) = validate_clocks(fclk, uclk, None, Some(4100.0)).unwrap();
        assert_eq!(f, 2000.0);
        assert_eq!(u, 2050.0);
    }

    #[test]
    fn validate_rejects_nonsense() {
        assert_eq!(
            validate_clocks(50.0, 2050.0, None, Some(4100.0)).unwrap_err(),
            "FCLK 50.0 MHz out of range"
        );
    }

    #[test]
    fn expected_version_constant() {
        assert_eq!(EXPECTED_TABLE_VERSION, 0x620105);
    }

    // -- test_raphael.py: the clock half --------------------------------------

    const RAPHAEL: &str = "AMD Ryzen 7 7800X3D 8-Core Processor";
    const GRANITE_RIDGE: &str = "AMD Ryzen 7 9850X3D 8-Core Processor";

    struct MailboxState {
        arg0: u32,
        arg1: u32,
        response: u32,
        commands: Vec<u32>,
        selector: Option<u32>,
    }

    /// Just enough of InpOutRsmuClockAccess to run read_clocks.
    pub(crate) struct FakeMailbox {
        version: u32,
        base: u64,
        dwords: BTreeMap<u64, u32>,
        state: Mutex<MailboxState>,
    }

    use std::collections::BTreeMap;

    impl FakeMailbox {
        pub(crate) fn new(version: u32, base: u64, dwords: &[(u64, u32)]) -> Self {
            FakeMailbox {
                version,
                base,
                dwords: dwords.iter().copied().collect(),
                state: Mutex::new(MailboxState { arg0: 0, arg1: 0, response: 1, commands: Vec::new(), selector: None }),
            }
        }

        pub(crate) fn commands(&self) -> Vec<u32> {
            self.state.lock().unwrap().commands.clone()
        }
    }

    impl RsmuAccess for FakeMailbox {
        fn is_driver_open(&self) -> bool {
            true
        }
        fn read_vendor(&self) -> HwResult<u32> {
            Ok(0x1022)
        }
        fn capture_selector(&self) -> HwResult<()> {
            self.state.lock().unwrap().selector = Some(0x1234);
            Ok(())
        }
        fn selector_captured(&self) -> bool {
            self.state.lock().unwrap().selector.is_some()
        }
        fn read_response(&self) -> HwResult<u32> {
            Ok(self.state.lock().unwrap().response)
        }
        fn clear_response(&self) -> HwResult<()> {
            self.state.lock().unwrap().response = 0;
            Ok(())
        }
        fn write_arguments(&self, _values: &[u32]) -> HwResult<()> {
            Ok(())
        }
        fn issue_command(&self, command: u32) -> HwResult<()> {
            let mut state = self.state.lock().unwrap();
            state.commands.push(command);
            if command == 0x05 {
                state.arg0 = self.version;
            } else if command == 0x04 {
                state.arg0 = (self.base & 0xFFFF_FFFF) as u32;
                state.arg1 = (self.base >> 32) as u32;
            }
            state.response = 1;
            Ok(())
        }
        fn read_arg0(&self) -> HwResult<u32> {
            Ok(self.state.lock().unwrap().arg0)
        }
        fn read_arg1(&self) -> HwResult<u32> {
            Ok(self.state.lock().unwrap().arg1)
        }
        fn restore_selector(&self) -> HwResult<()> {
            Ok(())
        }
        fn read_phys_dword(&self, address: u64) -> HwResult<u32> {
            Ok(self.dwords.get(&(address - self.base)).copied().unwrap_or(0))
        }
    }

    fn mailbox(version: u32, fclk: f32, uclk: f32, mclk: f32, offsets: (u64, u64, u64)) -> Arc<FakeMailbox> {
        Arc::new(FakeMailbox::new(
            version,
            0x1000_0000,
            &[(offsets.0, as_dword(fclk)), (offsets.1, as_dword(uclk)), (offsets.2, as_dword(mclk))],
        ))
    }

    fn raphael_mailbox() -> Arc<FakeMailbox> {
        mailbox(0x540108, 2000.0, 3000.0, 3000.0, (0x118, 0x128, 0x138))
    }

    fn reader(mailbox: Arc<FakeMailbox>, tables: &'static ClockTables, umc_mclk: Option<f64>) -> RsmuClockReader {
        let options = RsmuOptions {
            mutex: Some(Box::new(FakeMutex::default())),
            umc_mclk_mhz: umc_mclk,
            tables: Some(tables),
            ..RsmuOptions::default()
        };
        RsmuClockReader::with_options(mailbox, options).unwrap()
    }

    #[test]
    fn each_cpu_gets_only_its_own_tables() {
        assert!(std::ptr::eq(clock_tables_for_cpu(RAPHAEL), RAPHAEL_CLOCK_TABLES));
        assert!(std::ptr::eq(clock_tables_for_cpu(GRANITE_RIDGE), GRANITE_RIDGE_CLOCK_TABLES));
        assert!(clock_tables_for_cpu("AMD Ryzen 7 8700G w/ Radeon 780M Graphics").is_empty());
        for (version, _) in RAPHAEL_CLOCK_TABLES {
            assert!(clock_layout(GRANITE_RIDGE_CLOCK_TABLES, *version).is_none());
        }
    }

    #[test]
    fn the_tables_match_zenstates_core() {
        // PowerTable.cs, 8979d27: the 0x5400xx and 0x5401xx families keep
        // their clocks at 0x118/0x128/0x138; 0x540208 moved them up by 4.
        let low = clock_layout(RAPHAEL_CLOCK_TABLES, 0x540108).unwrap();
        assert_eq!((low.fclk, low.uclk, low.mclk, low.length), (0x118, 0x128, 0x138, 0x6BC));
        let high = clock_layout(RAPHAEL_CLOCK_TABLES, 0x540208).unwrap();
        assert_eq!((high.fclk, high.uclk, high.mclk, high.length), (0x11C, 0x12C, 0x13C, 0x8D0));
        assert!(RAPHAEL_CLOCK_TABLES.iter().all(|(_, layout)| !layout.verified));
        assert!(GRANITE_RIDGE_CLOCK_TABLES.iter().all(|(_, layout)| layout.verified));
    }

    #[test]
    fn a_raphael_table_reads_and_says_it_is_unverified() {
        let clocks = reader(raphael_mailbox(), RAPHAEL_CLOCK_TABLES, Some(3000.0)).read_clocks().unwrap();
        assert_eq!((clocks.fclk_mhz, clocks.uclk_mhz), (2000.0, 3000.0));
        assert!(!clocks.verified);
    }

    #[test]
    fn a_ryzen_9000_table_is_refused_on_raphael() {
        let mailbox = mailbox(0x620105, 2000.0, 3000.0, 3000.0, (0x11C, 0x12C, 0x13C));
        let rsmu = reader(mailbox.clone(), RAPHAEL_CLOCK_TABLES, Some(3000.0));
        assert!(rsmu.read_clocks().is_none());
        assert!(rsmu.last_error().contains("not an approved version"));
        // Refused at the version: the table was never transferred.
        assert!(!mailbox.commands().contains(&0x03));
    }

    #[test]
    fn an_unverified_table_needs_the_umc_cross_check() {
        let rsmu = reader(raphael_mailbox(), RAPHAEL_CLOCK_TABLES, None);
        assert!(rsmu.read_clocks().is_none());
        assert!(rsmu.last_error().contains("unverified"));
    }

    #[test]
    fn a_wrong_offset_is_caught_by_the_cross_check() {
        // What a misplaced offset looks like: a clock-shaped number that is
        // not the memory clock the UMC reports.
        let rsmu = reader(mailbox(0x540108, 2000.0, 2400.0, 3000.0, (0x118, 0x128, 0x138)), RAPHAEL_CLOCK_TABLES, Some(3000.0));
        assert!(rsmu.read_clocks().is_none());
        assert!(rsmu.last_error().contains("does not match"));
    }

    // -- Mailbox timing and frame, beyond the Python tests ------------------

    #[test]
    fn the_refusal_lists_the_versions_the_cpu_accepts() {
        let rsmu = reader(raphael_mailbox(), GRANITE_RIDGE_CLOCK_TABLES, Some(3000.0));
        assert!(rsmu.read_clocks().is_none());
        assert_eq!(
            rsmu.last_error(),
            "RSMU clock read failed: PM-table version 0x540108 is not an approved version for this CPU (0x620105)"
        );
    }

    #[test]
    fn a_silent_mailbox_times_out_on_the_fake_clock() {
        struct Silent(FakeMailbox);
        impl RsmuAccess for Silent {
            fn is_driver_open(&self) -> bool { true }
            fn read_vendor(&self) -> HwResult<u32> { Ok(0x1022) }
            fn capture_selector(&self) -> HwResult<()> { self.0.capture_selector() }
            fn selector_captured(&self) -> bool { self.0.selector_captured() }
            fn read_response(&self) -> HwResult<u32> { Ok(0) }
            fn clear_response(&self) -> HwResult<()> { Ok(()) }
            fn write_arguments(&self, _values: &[u32]) -> HwResult<()> { Ok(()) }
            fn issue_command(&self, command: u32) -> HwResult<()> { self.0.issue_command(command) }
            fn read_arg0(&self) -> HwResult<u32> { Ok(0) }
            fn read_arg1(&self) -> HwResult<u32> { Ok(0) }
            fn restore_selector(&self) -> HwResult<()> { Ok(()) }
            fn read_phys_dword(&self, _address: u64) -> HwResult<u32> { Ok(0) }
        }
        let ticks = Arc::new(AtomicU32::new(0));
        let clock = ticks.clone();
        let options = RsmuOptions {
            mutex: Some(Box::new(FakeMutex::default())),
            monotonic: Box::new(move || clock.fetch_add(1, Ordering::SeqCst) as f64 * 0.01),
            sleep: Box::new(|_| {}),
            ..RsmuOptions::default()
        };
        let access = Arc::new(Silent(FakeMailbox::new(0, 0, &[])));
        let rsmu = RsmuClockReader::with_options(access, options).unwrap();
        assert!(rsmu.read_clocks().is_none());
        assert_eq!(rsmu.last_error(), "RSMU clock read failed: RSMU mailbox response timed out");
    }

    #[test]
    fn out_of_range_timeouts_are_refused() {
        let options = RsmuOptions { timeout: 0.5, ..RsmuOptions::default() };
        assert_eq!(
            RsmuClockReader::with_options(raphael_mailbox(), options).err().unwrap(),
            "RSMU timeout must be finite and within (0, 0.25]"
        );
        let options = RsmuOptions { total_timeout: f64::NAN, ..RsmuOptions::default() };
        assert_eq!(
            RsmuClockReader::with_options(raphael_mailbox(), options).err().unwrap(),
            "RSMU total timeout must be finite and within (0, 1.0]"
        );
    }

    #[test]
    fn the_real_access_refuses_unpermitted_commands_and_writes_only_the_window() {
        let table = crate::hardware::pci_mcfg::sample_table(&[(0xE000_0000, 0, 0, 0xFF)]);
        let access = InpOutRsmuClockAccess::from_table(&table, 0, 0).unwrap();
        let fake = crate::testing::FakeHardware::new().install();
        assert_eq!(access.issue_command(0x6C).unwrap_err().0, "command 0x6C is not permitted");
        assert!(fake.log().is_empty());
        assert_eq!(access.restore_selector().unwrap_err().0, "No prior SMN selector was captured");
        access.issue_command(0x05).unwrap();
        assert_eq!(
            fake.log(),
            vec![
                crate::testing::Access::SetPhysLong(0xE000_0060, RSMU_MSG),
                crate::testing::Access::SetPhysLong(0xE000_0064, 0x05),
            ]
        );
        assert_eq!(access.last_written_selector(), Some(RSMU_MSG));
        assert_eq!(access.write_arguments(&[0; 5]).unwrap_err().0, "RSMU argument vector must have 6 DWORDs");
    }

    #[test]
    fn no_cpu_tables_means_no_read() {
        assert!(read_smu_clocks("AMD Ryzen 7 8700G w/ Radeon 780M Graphics", Some(3000.0)).is_none());
        // Granite Ridge with no MCFG table under test: the access cannot be
        // built, so the read is refused rather than attempted.
        assert!(read_smu_clocks(GRANITE_RIDGE, None).is_none());
    }
}
