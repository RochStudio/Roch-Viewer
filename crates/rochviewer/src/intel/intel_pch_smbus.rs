//! Read-only Intel PCH SMBus access, scoped to DDR4 temperature sensors.
//!
//! Fourth transport in this project. The DIMM temperature sensor answers
//! nowhere else: it is a device on the board's SMBus, not a register in the
//! memory controller, and the Super I/O sensor block does not carry it either.
//!
//! The host controller is register-compatible with the AMD FCH one this
//! project already drives, so the transaction loop is the ordinary PIIX4 one.
//! Only the base-address discovery differs: AMD publishes it through the PMIO
//! index/data ports, Intel through the SMBus function's own PCI configuration
//! space.
//!
//! That configuration space is reached through the firmware MCFG/ECAM window
//! rather than the legacy CF8/CFC ports, for the same reason the AM5 backend
//! does: measured on the Z790 target, CF8/CFC returned 0xFFFFFFFF for every
//! device including the 00:00.0 host bridge, which always exists. The ports
//! are not a usable transport here, so they are not used at all.
//!
//! SAFETY - this module reads, plus narrowly fixed selector writes:
//!
//!   * Every read assembles the transmit-slave-address byte with the read
//!     direction bit set, from a code path that has no direction parameter.
//!     Reads and the write are separate primitives, so no caller can turn a
//!     read into a write by passing an argument.
//!   * Two device classes, two read protocols, two allowlists, so widening one
//!     cannot widen the other. Word Data reaches the JEDEC thermal sensors at
//!     0x18-0x1F; Byte Data reaches the DDR5 PMIC and SPD hub at 0x48-0x57.
//!   * Exactly one writable target exists: the PMIC's ADC channel selector at
//!     R30h, on PMIC addresses only. [`WRITE_ALLOWLIST`] is the whole permitted
//!     set, tested as an equality on the `(address, register)` pair rather
//!     than a range. R30h is a selector and not a setting -- it picks which
//!     channel R31h converts, and cannot move a rail. The VID registers
//!     0x21/0x25/0x27 that do move VDD/VDDQ/VPP, and the mode register 0x2B,
//!     appear in no allowlist and cannot be written from here.
//!   * The SPD hubs at 0x50-0x57 take exactly one write, and not through
//!     `write_byte`, which refuses them outright: `select_spd_page` names the
//!     page register itself rather than accepting one, so nothing can steer a
//!     write at the EEPROM window, where it would corrupt a module's SPD.
//!   * DDR4's EE1004 EEPROM page is selected only through
//!     `select_ddr4_spd_page`. The caller supplies page 0 or 1; the method
//!     maps it to JEDEC's fixed SPA0/SPA1 addresses 0x36/0x37 and always sends
//!     data byte zero. No EEPROM address or register is accepted from the
//!     caller.
//!   * That write exists because the PMIC's measured rails cannot be reached
//!     without it. The ADC is a multiplexer: R30h selects a channel and R31h
//!     holds the sample, so a read-only transport can report what the rails
//!     are *set* to and never what they measure.
//!   * Access is serialised on the named mutexes the mainstream monitoring
//!     tools take for SMBus and for PCI configuration space.
//!   * The controller's status register is cleared on the way out, including
//!     on error, so the next owner of the mutex inherits a clean controller.
//!
//! The reader implements [`SmbusReader`], the contract the DDR5 device
//! readers drive either platform's bus through.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::hardware::pci_mcfg::{McfgAllocation, default_allocation, ecam_address};
use crate::hardware::{self, Hardware};
use crate::memory::ddr5_telemetry::{
    BusMutex, NamedBusMutex, SmbusError, SmbusOptions, SmbusReader, SpdBytes, monotonic, sleep_seconds,
};

// Mutex the mainstream monitoring tools take before driving SMBus.
pub const SMBUS_MUTEX_NAME: &str = "Global\\Access_SMBUS.HTP.Method";
pub const PCI_MUTEX_NAME: &str = "Global\\Access_PCI";

// The SMBus controller's fixed location. 100-series and later put it on
// function 4; older chipsets used function 3, so both are tried and each is
// identity-checked before use.
pub const SMBUS_BUS: u8 = 0x00;
pub const SMBUS_DEVICE: u8 = 0x1F;
pub const SMBUS_FUNCTIONS: [u8; 2] = [4, 3];

pub const INTEL_VENDOR_ID: u32 = 0x8086;
// PCI base class 0x0C (serial bus), sub-class 0x05 (SMBus).
pub const SMBUS_CLASS_CODE: u32 = 0x0C05;

pub const PCI_VENDOR_OFFSET: u16 = 0x00;
pub const PCI_COMMAND_OFFSET: u16 = 0x04;
pub const PCI_CLASS_OFFSET: u16 = 0x08;
pub const PCI_BAR4_OFFSET: u16 = 0x20;

pub const PCI_COMMAND_IO_SPACE: u32 = 0x0001;
pub const BAR_IO_SPACE_FLAG: u32 = 0x0001;
pub const SMBUS_BASE_MASK: u32 = 0xFFE0;

// Host controller register offsets, identical to the FCH layout.
pub const REG_HOST_STATUS: u16 = 0x00;
pub const REG_HOST_CONTROL: u16 = 0x02;
pub const REG_HOST_COMMAND: u16 = 0x03;
pub const REG_HOST_ADDRESS: u16 = 0x04;
pub const REG_HOST_DATA0: u16 = 0x05;
pub const REG_HOST_DATA1: u16 = 0x06;

// Transfer protocols. Word Data is what a JC-42.4 sensor answers with; Byte
// Data is how a DDR5 PMIC and SPD hub answer, and is also what the one
// allowlisted write uses. Read and write differ by the direction bit in the
// address byte, not by the protocol code, so there is no separate
// write-protocol constant to widen.
pub const PROTOCOL_WORD_DATA: u8 = 0x0C;
pub const PROTOCOL_BYTE_DATA: u8 = 0x08;
// Send/Receive Byte. DDR4 EE1004 uses Send Byte to select one of its two
// 256-byte pages at the fixed SPA0/SPA1 addresses below.
pub const PROTOCOL_BYTE: u8 = 0x04;
// Process Call: two bytes out, two back, in one transaction. Used on exactly
// one register; see select_spd_page for why it has to be this protocol.
pub const PROTOCOL_PROC_CALL: u8 = 0x10;
pub const CONTROL_START: u8 = 0x40;

pub const STATUS_HOST_BUSY: u8 = 0x01;
pub const STATUS_INTR: u8 = 0x02;
pub const STATUS_DEV_ERR: u8 = 0x04;
pub const STATUS_BUS_COLLISION: u8 = 0x08;
pub const STATUS_FAILED: u8 = 0x10;
pub const STATUS_ERROR_MASK: u8 = STATUS_DEV_ERR | STATUS_BUS_COLLISION | STATUS_FAILED;
pub const STATUS_CLEAR_MASK: u8 = 0x1F;

// JEDEC JC-42.4 thermal sensors. One address per slot, eight slots maximum.
// This list governs the Word Data path and nothing else.
pub const THERMAL_SENSOR_ADDRESSES: [u8; 8] = [0x18, 0x19, 0x1A, 0x1B, 0x1C, 0x1D, 0x1E, 0x1F];
pub const ALLOWED_ADDRESSES: [u8; 8] = THERMAL_SENSOR_ADDRESSES;

// DDR5 devices, reached with the Byte Data path. Deliberately a separate list
// from ALLOWED_ADDRESSES: different devices, different protocol, different
// allowlist, so neither can be widened by an edit meant for the other.
//
// A DDR5 module does not carry a JC-42.4 sensor at all -- the thermal sensor
// moved inside the SPD5 hub, which is why the 0x18-0x1F sweep finds nothing on
// a DDR5 board and the DIMM rows above it stay blank.
pub const SPD_HUB_ADDRESSES: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];
pub const PMIC_ADDRESSES: [u8; 8] = [0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F];
pub const DDR5_ADDRESSES: [u8; 16] = {
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 8 {
        out[i] = SPD_HUB_ADDRESSES[i];
        out[8 + i] = PMIC_ADDRESSES[i];
        i += 1;
    }
    out
};

// The PCH exposes one SMBus controller where an AM5 FCH exposes two. Named and
// accepted so a caller written against either transport passes the same
// arguments; anything else is refused rather than silently treated as zero.
pub const CONTROLLER_OFFSETS: [u8; 1] = [0x00];

// DDR4 EE1004 page selectors. A Send Byte to 0x36 selects page 0; the same
// transaction to 0x37 selects page 1. These addresses never expose EEPROM
// data, so the selector cannot be redirected into the writable SPD array.
pub const DDR4_SPD_PAGE_SELECT_ADDRESSES: [u8; 2] = [0x36, 0x37];
pub const DDR4_SPD_PAGE_SIZE: i64 = 0x100;

// The PMIC's ADC channel selector.
pub const PMIC_TELEMETRY_SELECT_REGISTER: u8 = 0x30;

// The SPD5 hub's page selector, MR11, and the geometry it selects. The module
// identity lives at byte 0x200 and the hub exposes 128 bytes at a time, so the
// page has to be selected before the read.
//
// Selecting it is a write, and the PCH may be told to refuse writes here. Its
// SMBus Host Configuration register (D31:F4 offset 0x40) carries SPD Write
// Disable at bit 4, which on this board's BIOS defaults to enabled. The bit is
// set-once: firmware arms it during POST and nothing in the running OS can
// clear it, through either config decode path -- measured on the bench, where
// setting it took effect and clearing it did not.
//
// It gates on the transaction's direction bit rather than on whether bytes
// reach the device, so a Process Call -- which the controller issues with that
// bit set, yet which still carries a write phase -- selects the page with the
// interlock armed. See select_spd_page, which is the only thing here that
// writes to a hub, and which reaches exactly this one register.
pub const SPD_HUB_PAGE_REGISTER: u8 = 0x0B;
pub const SPD_HUB_PAGE_MASK: u8 = 0x07; // 8 pages of 128 bytes
pub const SPD_EEPROM_WINDOW: u8 = 0x80; // register bit 7 set => EEPROM, not MR space
pub const SPD_PAGE_SIZE: i64 = 0x80;

// The complete set of writable targets: {(address, register)}.
//
// One register, on PMIC addresses only: the ADC channel selector, which
// chooses what a following read returns and stores nothing.
//
// The SPD hubs are deliberately absent. They are written -- the page has to be
// selected -- but not through here: that goes through select_spd_page, which
// is fixed to one register and cannot be pointed at the EEPROM window, where a
// stray write would corrupt a module's SPD.
pub const WRITE_ALLOWLIST: [(u8, u8); 8] = {
    let mut out = [(0u8, 0u8); 8];
    let mut i = 0;
    while i < 8 {
        out[i] = (PMIC_ADDRESSES[i], PMIC_TELEMETRY_SELECT_REGISTER);
        i += 1;
    }
    out
};

// Registers that must never be writable, asserted by the test suite. The first
// three set VDD/VDDQ/VPP; 0x2B selects which VID encoding they use, which
// moves the same rails by reinterpreting them.
pub const PMIC_RAIL_CONTROL_REGISTERS: [u8; 4] = [0x21, 0x25, 0x27, 0x2B];

/// Physical dword reader built on the bundled InpOut mapping.
fn read_dword(address: u64) -> Result<u32, SmbusError> {
    match crate::hardware::read::read_physical_memory_int(address, 4) {
        Some(value) => Ok((value & 0xFFFF_FFFF) as u32),
        None => Err(SmbusError::Bus(format!("physical read failed at 0x{address:016X}"))),
    }
}

/// The firmware's ECAM allocation covering the SMBus controller.
///
/// The lookup itself is platform-neutral and lives with the rest of the MCFG
/// parsing; this keeps the name the SMBus code has always called.
pub fn default_ecam_allocation() -> Result<McfgAllocation, String> {
    default_allocation(0, SMBUS_BUS)
}

/// Read one dword from the SMBus function's PCI configuration space.
fn config_dword(allocation: &McfgAllocation, function: u8, offset: u16) -> Result<u32, SmbusError> {
    let address = ecam_address(allocation, SMBUS_BUS, SMBUS_DEVICE, function, offset).map_err(SmbusError::Refused)?;
    read_dword(address)
}

/// The controller's I/O base, or [`SmbusError::Unavailable`].
///
/// The function is identified before its BAR is trusted: an Intel vendor ID
/// and the SMBus class code both have to match, so a chipset that puts
/// something else at 00:1F.4 cannot be driven as if it were a bus controller.
///
/// `allocation` defaults to the firmware's, `mutex` to the shared PCI
/// configuration mutex.
pub fn find_smbus_base(allocation: Option<McfgAllocation>, mutex: Option<&dyn BusMutex>) -> Result<u16, SmbusError> {
    let allocation = match allocation {
        Some(allocation) => allocation,
        None => default_ecam_allocation().map_err(SmbusError::Bus)?,
    };
    let named;
    let mutex: &dyn BusMutex = match mutex {
        Some(mutex) => mutex,
        None => {
            named = NamedBusMutex::new(PCI_MUTEX_NAME);
            &named
        }
    };

    let mut reasons = Vec::new();
    {
        let _held = mutex.acquire()?;
        for function in SMBUS_FUNCTIONS {
            let identity = config_dword(&allocation, function, PCI_VENDOR_OFFSET)?;
            if identity == 0xFFFF_FFFF || identity == 0x0000_0000 {
                reasons.push(format!("00:1F.{function} absent"));
                continue;
            }
            if (identity & 0xFFFF) != INTEL_VENDOR_ID {
                reasons.push(format!("00:1F.{function} vendor 0x{:04X}", identity & 0xFFFF));
                continue;
            }

            let class_dword = config_dword(&allocation, function, PCI_CLASS_OFFSET)?;
            if ((class_dword >> 16) & 0xFFFF) != SMBUS_CLASS_CODE {
                reasons.push(format!("00:1F.{function} class 0x{:04X}", (class_dword >> 16) & 0xFFFF));
                continue;
            }

            let command = config_dword(&allocation, function, PCI_COMMAND_OFFSET)?;
            if command & PCI_COMMAND_IO_SPACE == 0 {
                reasons.push(format!("00:1F.{function} I/O space disabled"));
                continue;
            }

            let bar = config_dword(&allocation, function, PCI_BAR4_OFFSET)?;
            if bar & BAR_IO_SPACE_FLAG == 0 {
                reasons.push(format!("00:1F.{function} BAR4 is not I/O"));
                continue;
            }

            let base = bar & SMBUS_BASE_MASK;
            if base == 0 {
                reasons.push(format!("00:1F.{function} BAR4 unassigned"));
                continue;
            }
            return Ok(base as u16);
        }
    }

    if reasons.is_empty() {
        reasons.push("none".to_string());
    }
    Err(SmbusError::Unavailable(format!("No Intel SMBus controller found ({})", reasons.join("; "))))
}

/// The backend's reader factory: `PchSmbusReader()` with every default.
pub fn new_reader() -> Result<Arc<dyn SmbusReader>, SmbusError> {
    Ok(Arc::new(PchSmbusReader::new()?))
}

fn hw_error(error: hardware::HwError) -> SmbusError {
    SmbusError::Bus(error.0)
}

fn check_controller(controller_offset: u8) -> Result<(), SmbusError> {
    if CONTROLLER_OFFSETS.contains(&controller_offset) {
        Ok(())
    } else {
        Err(SmbusError::Refused(format!("Unknown SMBus controller offset 0x{controller_offset:02X}")))
    }
}

/// Serialized, allowlisted, read-only SMBus word reader.
pub struct PchSmbusReader {
    io: Arc<dyn Hardware>,
    mutex: Box<dyn BusMutex>,
    lock: Mutex<()>,
    monotonic: Box<dyn Fn() -> f64 + Send + Sync>,
    sleep: Box<dyn Fn(f64) + Send + Sync>,
    timeout: f64,
    pub base: u16,
    last_error: Mutex<String>,
}

impl PchSmbusReader {
    /// The real controller: the driver, the shared SMBus mutex, and the base
    /// found in the SMBus function's configuration space.
    pub fn new() -> Result<Self, SmbusError> {
        Self::with_options(SmbusOptions::default())
    }

    pub fn with_options(options: SmbusOptions) -> Result<Self, SmbusError> {
        // These are the project's platform-neutral port-I/O primitives;
        // building them fails when the driver could not be loaded.
        if let Some(reason) = hardware::driver_error() {
            return Err(SmbusError::Bus(reason));
        }
        let io = hardware::hw();
        let mutex = options.mutex.unwrap_or_else(|| Box::new(NamedBusMutex::new(SMBUS_MUTEX_NAME)));
        let timeout = options.timeout;
        if !(0.0 < timeout && timeout <= 0.25) {
            return Err(SmbusError::Refused("SMBus timeout must be within (0, 0.25]".into()));
        }
        let base = match options.base {
            Some(base) => base,
            None => find_smbus_base(None, None)?,
        };
        Ok(PchSmbusReader {
            io,
            mutex,
            lock: Mutex::new(()),
            monotonic: options.monotonic.unwrap_or_else(|| Box::new(monotonic)),
            sleep: options.sleep.unwrap_or_else(|| Box::new(sleep_seconds)),
            timeout,
            base,
            last_error: Mutex::new(String::new()),
        })
    }

    /// Why the last SPD page restore failed, or "".
    pub fn last_error(&self) -> String {
        self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set_last_error(&self, message: String) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    fn hold(&self) -> MutexGuard<'_, ()> {
        self.lock.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn inb(&self, port: u16) -> Result<u8, SmbusError> {
        self.io.inb(port).map_err(hw_error)
    }

    fn outb(&self, port: u16, value: u8) -> Result<(), SmbusError> {
        self.io.outb(port, value).map_err(hw_error)
    }

    fn wait_not_busy(&self, port_status: u16) -> Result<u8, SmbusError> {
        let deadline = (self.monotonic)() + self.timeout;
        loop {
            let status = self.inb(port_status)?;
            if status & STATUS_HOST_BUSY == 0 {
                return Ok(status);
            }
            if (self.monotonic)() > deadline {
                return Err(SmbusError::Timeout("SMBus host stayed busy".into()));
            }
            (self.sleep)(0.0002);
        }
    }

    fn wait_complete(&self, port_status: u16) -> Result<u8, SmbusError> {
        let deadline = (self.monotonic)() + self.timeout;
        loop {
            let status = self.inb(port_status)?;
            if status & STATUS_ERROR_MASK != 0 {
                return Err(SmbusError::Bus(format!("SMBus transfer error, status 0x{status:02X}")));
            }
            if status & STATUS_INTR != 0 && status & STATUS_HOST_BUSY == 0 {
                return Ok(status);
            }
            if (self.monotonic)() > deadline {
                return Err(SmbusError::Timeout("SMBus transfer did not complete".into()));
            }
            (self.sleep)(0.0002);
        }
    }

    /// Read a register with the Read Word protocol.
    ///
    /// Returns `(first_byte, second_byte)` in the order the device put them
    /// on the wire, leaving byte order to the caller: SMBus sends the low byte
    /// first, but a JC-42.4 sensor answers most-significant byte first, and
    /// only the device's own specification says which applies.
    ///
    /// Fails on refusal, timeout or bus error; never returns a partial value.
    pub fn read_word_bytes(&self, address: u8, register: u8) -> Result<(u8, u8), SmbusError> {
        if !ALLOWED_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!(
                "SMBus address 0x{address:02X} is outside the thermal-sensor range"
            )));
        }
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.read_word_locked(address, register)
    }

    /// Read a DDR5 device register with the Read Byte Data protocol.
    ///
    /// The signature matches the AM5 transport's so one caller can drive
    /// either bus without knowing which platform it is on.
    ///
    /// Fails on refusal, timeout or bus error; never returns a partial value.
    pub fn read_byte(&self, address: u8, register: u8, controller_offset: u8) -> Result<u8, SmbusError> {
        if !DDR5_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!(
                "SMBus address 0x{address:02X} is outside the DDR5 SPD/PMIC allowlist"
            )));
        }
        check_controller(controller_offset)?;
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.read_byte_locked(address, register)
    }

    /// Read SPD EEPROM bytes across page boundaries.
    ///
    /// Restores whichever page the hub was left on, so a tool reading the
    /// module after this one finds it as it was.
    ///
    /// The whole sequence holds the bus mutex, rather than taking it per
    /// byte. It has to: which bytes the EEPROM window returns depends on a
    /// page selected by an earlier transaction, so releasing the mutex
    /// between them lets another master repage the hub underneath the read.
    /// That is not hypothetical -- running this beside a second tool doing
    /// the same thing returned a part number ending "Kz>>>}", bytes read at
    /// the right offsets of the wrong page. Per-byte locking is safe only for
    /// reads that carry their own address.
    pub fn read_spd(
        &self,
        address: u8,
        offset: i64,
        length: i64,
        controller_offset: u8,
    ) -> Result<SpdBytes, SmbusError> {
        if !SPD_HUB_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!("0x{address:02X} is not an SPD hub address")));
        }
        check_controller(controller_offset)?;
        let mut values = SpdBytes::new();
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        let original_page = self.read_byte_locked(address, SPD_HUB_PAGE_REGISTER)?;
        let mut current_page: Option<i64> = None;
        let result = (|| -> Result<(), SmbusError> {
            for position in offset..offset + length {
                let (page, within) = (position.div_euclid(SPD_PAGE_SIZE), position.rem_euclid(SPD_PAGE_SIZE));
                if page & !(SPD_HUB_PAGE_MASK as i64) != 0 {
                    break;
                }
                if current_page != Some(page) {
                    // A refusal here is the platform refusing the
                    // transaction outright, not a transient error, so it
                    // stops the read rather than being retried for every
                    // byte of every page.
                    self.select_page_locked(address, page as u8)?;
                    current_page = Some(page);
                }
                match self.read_byte_locked(address, SPD_EEPROM_WINDOW | within as u8) {
                    Ok(value) => {
                        values.insert(position as u32, value);
                    }
                    Err(error) if error.is_io() => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        match self.select_page_locked(address, original_page) {
            Ok(()) => {}
            Err(error) if error.is_io() => self.set_last_error(format!("SPD page restore failed: {error}")),
            Err(error) => return Err(error),
        }
        result?;
        Ok(values)
    }

    /// Read an EE1004 DDR4 SPD range and restore the conventional page 0.
    ///
    /// Page selection and every dependent byte read share one lock. The only
    /// write is a Send Byte to JEDEC's fixed SPA0/SPA1 selector addresses;
    /// no write is sent to the module's 0x50-0x57 EEPROM address.
    pub fn read_ddr4_spd(
        &self,
        address: u8,
        offset: i64,
        length: i64,
        controller_offset: u8,
    ) -> Result<SpdBytes, SmbusError> {
        if !SPD_HUB_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!("0x{address:02X} is not an SPD EEPROM address")));
        }
        check_controller(controller_offset)?;
        if offset < 0 || length < 0 || offset + length > 2 * DDR4_SPD_PAGE_SIZE {
            return Err(SmbusError::Refused("DDR4 SPD range is outside the 512-byte EEPROM".into()));
        }

        let mut values = SpdBytes::new();
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        let mut current_page: Option<i64> = None;
        let result = (|| -> Result<(), SmbusError> {
            for position in offset..offset + length {
                let (page, within) = (position / DDR4_SPD_PAGE_SIZE, position % DDR4_SPD_PAGE_SIZE);
                if current_page != Some(page) {
                    self.select_ddr4_spd_page_locked(page)?;
                    current_page = Some(page);
                }
                match self.read_byte_locked(address, within as u8) {
                    Ok(value) => {
                        values.insert(position as u32, value);
                    }
                    Err(error) if error.is_io() => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        match self.select_ddr4_spd_page_locked(0) {
            Ok(()) => {}
            Err(error) if error.is_io() => self.set_last_error(format!("DDR4 SPD page restore failed: {error}")),
            Err(error) => return Err(error),
        }
        result?;
        Ok(values)
    }

    /// True when a device answers at `address`.
    ///
    /// Refusals, timeouts and bus errors all read as absent; they are the only
    /// failures a byte read has.
    pub fn probe(&self, address: u8, controller_offset: u8, register: u8) -> bool {
        self.read_byte(address, register, controller_offset).is_ok()
    }

    /// Write one byte, but only to a target in [`WRITE_ALLOWLIST`].
    ///
    /// The allowlist holds exactly one register on the PMIC addresses -- the
    /// ADC channel selector -- so no rail-control register, and no SPD hub
    /// address at all, is reachable through this method.
    pub fn write_byte(&self, address: u8, register: u8, value: u8, controller_offset: u8) -> Result<(), SmbusError> {
        if !WRITE_ALLOWLIST.contains(&(address, register)) {
            return Err(SmbusError::Refused(format!(
                "SMBus write to 0x{address:02X} register 0x{register:02X} is not permitted \
                 (only the PMIC telemetry selector is writable)"
            )));
        }
        check_controller(controller_offset)?;
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.write_byte_locked(address, register, value)
    }

    /// Write one byte. Caller must already hold the lock and the mutex.
    fn write_byte_locked(&self, address: u8, register: u8, value: u8) -> Result<(), SmbusError> {
        let base = self.base;
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        // The only transfer on this transport that clears the direction bit.
        self.outb(base + REG_HOST_ADDRESS, (((address as u16) << 1) & 0xFE) as u8)?;
        self.outb(base + REG_HOST_COMMAND, register)?;
        self.outb(base + REG_HOST_DATA0, value)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START)?;
        let result = self.wait_complete(port_status).map(|_| ());
        // Leave the controller clean for the next owner of the mutex.
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// Point an SPD hub's EEPROM window at one of its eight pages.
    ///
    /// Fixed to SPD_HUB_PAGE_REGISTER on an SPD hub address: the register is
    /// not a parameter, so nothing can steer this at the EEPROM window and
    /// write to the array itself. The value is masked to the three page bits.
    ///
    /// Issued as a Process Call rather than a byte write; see
    /// SPD_HUB_PAGE_REGISTER for why a byte write cannot reach the hub here.
    ///
    /// A Process Call sends a second data byte. On this hub it goes nowhere:
    /// dumping MR0-MR20 either side of the call shows MR11 moving and nothing
    /// else, so the block write-protection registers next to it are untouched.
    pub fn select_spd_page(&self, address: u8, page: u8) -> Result<(), SmbusError> {
        if !SPD_HUB_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!("0x{address:02X} is not an SPD hub address")));
        }
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.select_page_locked(address, page)
    }

    /// Select DDR4 SPD page 0 or 1 through the fixed EE1004 address.
    pub fn select_ddr4_spd_page(&self, page: i64, controller_offset: u8) -> Result<(), SmbusError> {
        check_controller(controller_offset)?;
        if !(0 <= page && page < DDR4_SPD_PAGE_SELECT_ADDRESSES.len() as i64) {
            return Err(SmbusError::Refused("DDR4 SPD page must be 0 or 1".into()));
        }
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.select_ddr4_spd_page_locked(page)
    }

    /// Issue EE1004's Send Byte page command while the bus is locked.
    fn select_ddr4_spd_page_locked(&self, page: i64) -> Result<(), SmbusError> {
        if !(0 <= page && page < DDR4_SPD_PAGE_SELECT_ADDRESSES.len() as i64) {
            return Err(SmbusError::Refused("DDR4 SPD page must be 0 or 1".into()));
        }
        let address = DDR4_SPD_PAGE_SELECT_ADDRESSES[page as usize];
        let base = self.base;
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        self.outb(base + REG_HOST_ADDRESS, (((address as u16) << 1) & 0xFE) as u8)?;
        self.outb(base + REG_HOST_COMMAND, 0x00)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_BYTE | CONTROL_START)?;
        let result = self.wait_complete(port_status).map(|_| ());
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// Select a page. Caller must already hold the lock and the mutex.
    fn select_page_locked(&self, address: u8, page: u8) -> Result<(), SmbusError> {
        let base = self.base;
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        // Direction bit set: on this path it is also what carries the write
        // past SPD Write Disable.
        self.outb(base + REG_HOST_ADDRESS, ((((address as u16) << 1) | 1) & 0xFF) as u8)?;
        self.outb(base + REG_HOST_COMMAND, SPD_HUB_PAGE_REGISTER)?;
        self.outb(base + REG_HOST_DATA0, page & SPD_HUB_PAGE_MASK)?;
        self.outb(base + REG_HOST_DATA1, 0x00)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_PROC_CALL | CONTROL_START)?;
        let result = self.wait_complete(port_status).map(|_| ());
        // Leave the controller clean for the next owner of the mutex.
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// Read one byte. Caller must already hold the lock and the mutex.
    fn read_byte_locked(&self, address: u8, register: u8) -> Result<u8, SmbusError> {
        let base = self.base;
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        // Direction bit is set to 1 (read) for every transfer on this path.
        self.outb(base + REG_HOST_ADDRESS, ((((address as u16) << 1) | 1) & 0xFF) as u8)?;
        self.outb(base + REG_HOST_COMMAND, register)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START)?;
        let result = self.wait_complete(port_status).and_then(|_| self.inb(base + REG_HOST_DATA0));
        // Leave the controller clean for the next owner of the mutex.
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// Read one word. Caller must already hold the lock and the mutex.
    fn read_word_locked(&self, address: u8, register: u8) -> Result<(u8, u8), SmbusError> {
        let base = self.base;
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        // Direction bit is set to 1 (read) for every transfer on this path.
        self.outb(base + REG_HOST_ADDRESS, ((((address as u16) << 1) | 1) & 0xFF) as u8)?;
        self.outb(base + REG_HOST_COMMAND, register)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_WORD_DATA | CONTROL_START)?;
        let result = self
            .wait_complete(port_status)
            .and_then(|_| Ok((self.inb(base + REG_HOST_DATA0)?, self.inb(base + REG_HOST_DATA1)?)));
        // Leave the controller clean for the next owner of the mutex.
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// The sensor addresses that answered, in slot order.
    ///
    /// An empty slot, or a module whose maker left the sensor off the board,
    /// simply does not answer; that is a normal outcome and not an error.
    pub fn responding_addresses(&self, register: u8) -> Vec<u8> {
        let mut found = Vec::new();
        for address in THERMAL_SENSOR_ADDRESSES {
            if self.read_word_bytes(address, register).is_err() {
                continue;
            }
            found.push(address);
        }
        found
    }
}

impl SmbusReader for PchSmbusReader {
    fn is_driver_open(&self) -> bool {
        self.io.is_driver_open()
    }

    fn read_byte(&self, address: u8, register: u8, controller: u8) -> Result<u8, SmbusError> {
        PchSmbusReader::read_byte(self, address, register, controller)
    }

    fn write_byte(&self, address: u8, register: u8, value: u8, controller: u8) -> Result<(), SmbusError> {
        PchSmbusReader::write_byte(self, address, register, value, controller)
    }

    fn read_spd(&self, address: u8, offset: i64, length: i64, controller: u8) -> Result<SpdBytes, SmbusError> {
        PchSmbusReader::read_spd(self, address, offset, length, controller)
    }

    fn read_ddr4_spd(&self, address: u8, offset: i64, length: i64, controller: u8) -> Result<SpdBytes, SmbusError> {
        PchSmbusReader::read_ddr4_spd(self, address, offset, length, controller)
    }

    fn probe_address(&self, address: u8, controller: u8) -> bool {
        self.probe(address, controller, 0x00)
    }

    fn read_word_bytes(&self, address: u8, register: u8) -> Result<(u8, u8), SmbusError> {
        PchSmbusReader::read_word_bytes(self, address, register)
    }

    fn responding_addresses(&self, register: u8) -> Result<Vec<u8>, SmbusError> {
        Ok(PchSmbusReader::responding_addresses(self, register))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::ddr5_telemetry::BusGuard;
    use crate::testing::{Access, FakeHardware};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ALLOCATION: McfgAllocation =
        McfgAllocation { base_address: 0xC000_0000, segment: 0, start_bus: 0, end_bus: 255 };
    const BASE: u16 = 0xEFA0;

    // What the Z790 target actually reports.
    const GOOD_FUNCTION_4: [(u16, u32); 4] = [
        (0x00, 0x7A23_8086), // Intel vendor, SMBus device
        (0x04, 0x0000_0003), // I/O space enabled
        (0x08, 0x0C05_0004), // class 0x0C05 in the upper half
        (0x20, 0x0000_EFA1), // BAR4, I/O space flag set
    ];
    // Function 3 on this chipset is audio, which must be rejected on class.
    const AUDIO_FUNCTION_3: [(u16, u32); 4] =
        [(0x00, 0x7A50_8086), (0x04, 0x0000_0003), (0x08, 0x0403_0004), (0x20, 0xFFF0_0004)];

    /// A fake config space at the ECAM addresses; anything else reads all
    /// ones, as an absent function does.
    fn install_config_space(by_function: &[(u8, &[(u16, u32)])]) {
        let mut fake = FakeHardware::new();
        fake.unmapped_reads_ones = true;
        for (function, registers) in by_function {
            for (offset, value) in registers.iter() {
                let address = ecam_address(&ALLOCATION, 0, 0x1F, *function, *offset).unwrap();
                fake = fake.with_dword(address, *value);
            }
        }
        fake.install();
    }

    fn with(registers: [(u16, u32); 4], offset: u16, value: u32) -> [(u16, u32); 4] {
        registers.map(|(o, v)| if o == offset { (o, value) } else { (o, v) })
    }

    /// Re-enterable stand-in: the real reader takes the mutex once per read.
    struct NullMutex;

    impl BusMutex for NullMutex {
        fn acquire(&self) -> Result<BusGuard, SmbusError> {
            Ok(Box::new(()))
        }
    }

    /// Records every port write so the read-only contract can be asserted:
    /// the status register answers from `status_sequence` (then INTR), the
    /// data registers from `data`, and every other port with zero.
    fn fake_io(status_sequence: Option<Vec<u8>>, data: (u8, u8)) -> Arc<FakeHardware> {
        let status = Mutex::new(VecDeque::from(status_sequence.unwrap_or_else(|| vec![0x00, STATUS_INTR])));
        FakeHardware::new()
            .with_port_reader(move |_fake, port, _width| {
                if port == BASE + REG_HOST_STATUS {
                    return Some(status.lock().unwrap().pop_front().unwrap_or(STATUS_INTR) as u32);
                }
                if port == BASE + REG_HOST_DATA0 {
                    return Some(data.0 as u32);
                }
                if port == BASE + REG_HOST_DATA1 {
                    return Some(data.1 as u32);
                }
                Some(0x00)
            })
            .install()
    }

    fn writes(io: &FakeHardware) -> Vec<(u16, u8)> {
        let log = io.log();
        // This transport must not touch the legacy ports.
        assert!(!log.iter().any(|a| matches!(a, Access::InL(_) | Access::OutL(..))));
        log.into_iter()
            .filter_map(|access| match access {
                Access::OutB(port, value) => Some((port, value)),
                _ => None,
            })
            .collect()
    }

    fn values_at(io: &FakeHardware, port: u16) -> Vec<u8> {
        writes(io).into_iter().filter(|(p, _)| *p == port).map(|(_, v)| v).collect()
    }

    fn first_at(io: &FakeHardware, port: u16) -> u8 {
        values_at(io, port)[0]
    }

    fn make_reader() -> PchSmbusReader {
        PchSmbusReader::with_options(SmbusOptions {
            mutex: Some(Box::new(NullMutex)),
            base: Some(BASE),
            sleep: Some(Box::new(|_| {})),
            ..SmbusOptions::default()
        })
        .unwrap()
    }

    // --- Ddr5ByteReadTest: the Byte Data path that reaches the DDR5 PMIC
    // and SPD hub.

    #[test]
    fn a_pmic_register_is_read_with_the_byte_protocol() {
        let io = fake_io(None, (0x8C, 0x00));
        // 0x48 R21h is SWA, the VDD rail: 0x8C decodes to 1.500 V on the bench.
        assert_eq!(make_reader().read_byte(0x48, 0x21, 0x00), Ok(0x8C));
        assert_eq!(first_at(&io, BASE + REG_HOST_CONTROL), PROTOCOL_BYTE_DATA | CONTROL_START);
    }

    #[test]
    fn ddr5_the_direction_bit_is_always_read() {
        let io = fake_io(None, (0x8C, 0x00));
        make_reader().read_byte(0x50, 0x31, 0x00).unwrap();
        let slave = first_at(&io, BASE + REG_HOST_ADDRESS);
        assert_eq!(slave & 0x01, 0x01);
        assert_eq!(slave >> 1, 0x50);
    }

    #[test]
    fn addresses_outside_the_ddr5_list_are_refused() {
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for address in [0x00, 0x18, 0x1F, 0x47, 0x58, 0x77] {
            assert!(matches!(reader.read_byte(address, 0x21, 0x00), Err(SmbusError::Refused(_))), "{address:#x}");
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn an_unknown_controller_offset_is_refused() {
        let io = fake_io(None, (0, 0));
        // 0x20 is the second AM5 controller; the PCH has no such thing, and
        // accepting it silently would read controller zero instead.
        assert!(matches!(make_reader().read_byte(0x48, 0x21, 0x20), Err(SmbusError::Refused(_))));
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn the_two_allowlists_stay_separate() {
        // Widening one device class must never widen the other.
        assert_eq!(ALLOWED_ADDRESSES.to_vec(), (0x18..0x20).collect::<Vec<u8>>());
        let mut ddr5 = DDR5_ADDRESSES.to_vec();
        ddr5.sort();
        assert_eq!(ddr5, (0x48..0x58).collect::<Vec<u8>>());
        assert!(!ALLOWED_ADDRESSES.iter().any(|a| DDR5_ADDRESSES.contains(a)));
    }

    #[test]
    fn a_thermal_sensor_is_unreachable_from_the_byte_path() {
        // The JC-42.4 sensors answer Word Data; keeping them off this path is
        // what stops the two allowlists collapsing into one over time.
        fake_io(None, (0, 0));
        assert!(matches!(make_reader().read_byte(0x18, 0x05, 0x00), Err(SmbusError::Refused(_))));
    }

    #[test]
    fn ddr5_the_controller_is_left_clean() {
        let io = fake_io(None, (0x8C, 0x00));
        make_reader().read_byte(0x48, 0x21, 0x00).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_STATUS).last(), Some(&STATUS_CLEAR_MASK));
    }

    #[test]
    fn ddr5_a_bus_error_raises_and_still_clears_status() {
        let io = fake_io(Some(vec![0x00, STATUS_DEV_ERR]), (0, 0));
        assert!(matches!(make_reader().read_byte(0x48, 0x21, 0x00), Err(SmbusError::Bus(_))));
        assert_eq!(values_at(&io, BASE + REG_HOST_STATUS).last(), Some(&STATUS_CLEAR_MASK));
    }

    #[test]
    fn ddr5_only_controller_registers_are_written() {
        let io = fake_io(None, (0x8C, 0x00));
        make_reader().read_byte(0x48, 0x21, 0x00).unwrap();
        let written: std::collections::BTreeSet<u16> = writes(&io).into_iter().map(|(p, _)| p - BASE).collect();
        assert_eq!(
            written,
            [REG_HOST_STATUS, REG_HOST_ADDRESS, REG_HOST_COMMAND, REG_HOST_CONTROL].into_iter().collect()
        );
    }

    // --- Ddr4SpdPageTest: EE1004 page selection can only reach SPA0/SPA1,
    // never EEPROM data.

    #[test]
    fn page_one_uses_send_byte_at_the_fixed_spa1_address() {
        let io = fake_io(None, (0, 0));
        make_reader().select_ddr4_spd_page(1, 0x00).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_ADDRESS), vec![(DDR4_SPD_PAGE_SELECT_ADDRESSES[1] << 1) & 0xFE]);
        assert_eq!(values_at(&io, BASE + REG_HOST_COMMAND), vec![0x00]);
        assert_eq!(values_at(&io, BASE + REG_HOST_CONTROL), vec![PROTOCOL_BYTE | CONTROL_START]);
    }

    #[test]
    fn only_pages_zero_and_one_are_accepted() {
        for page in [-1, 2, 255] {
            let io = fake_io(None, (0, 0));
            assert!(matches!(make_reader().select_ddr4_spd_page(page, 0x00), Err(SmbusError::Refused(_))));
            assert_eq!(writes(&io), vec![]);
        }
    }

    #[test]
    fn upper_page_read_restores_page_zero() {
        let io = fake_io(None, (0xA5, 0x00));
        let values = make_reader().read_ddr4_spd(0x51, 0x140, 2, 0x00).unwrap();
        assert_eq!(values, BTreeMap::from([(0x140, 0xA5), (0x141, 0xA5)]));
        let selector_addresses: Vec<u8> =
            values_at(&io, BASE + REG_HOST_ADDRESS).into_iter().filter(|v| v & 1 == 0).map(|v| v >> 1).collect();
        assert_eq!(selector_addresses, vec![0x37, 0x36]);
    }

    #[test]
    fn range_cannot_leave_the_512_byte_spd() {
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for (offset, length) in [(-1, 1), (0, -1), (0x1FF, 2)] {
            assert!(matches!(reader.read_ddr4_spd(0x50, offset, length, 0x00), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![]);
    }

    // --- BaseDiscoveryTest

    #[test]
    fn the_smbus_function_is_found_and_its_bar_masked() {
        install_config_space(&[(4, &GOOD_FUNCTION_4), (3, &AUDIO_FUNCTION_3)]);
        assert_eq!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Ok(BASE));
    }

    #[test]
    fn a_non_intel_vendor_is_refused() {
        install_config_space(&[(4, &with(GOOD_FUNCTION_4, 0x00, 0x7A23_1022))]);
        assert!(matches!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Err(SmbusError::Unavailable(_))));
    }

    #[test]
    fn a_function_with_the_wrong_class_is_refused() {
        install_config_space(&[(4, &AUDIO_FUNCTION_3)]);
        assert!(matches!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Err(SmbusError::Unavailable(_))));
    }

    #[test]
    fn function_three_is_used_when_it_is_the_smbus_one() {
        // Older chipsets put the controller on function 3.
        install_config_space(&[(3, &GOOD_FUNCTION_4)]);
        assert_eq!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Ok(BASE));
    }

    #[test]
    fn disabled_io_space_is_refused() {
        install_config_space(&[(4, &with(GOOD_FUNCTION_4, 0x04, 0x0000_0002))]);
        assert!(matches!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Err(SmbusError::Unavailable(_))));
    }

    #[test]
    fn a_memory_bar_is_refused() {
        install_config_space(&[(4, &with(GOOD_FUNCTION_4, 0x20, 0x0000_EFA0))]);
        assert!(matches!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Err(SmbusError::Unavailable(_))));
    }

    #[test]
    fn an_unassigned_bar_is_refused() {
        install_config_space(&[(4, &with(GOOD_FUNCTION_4, 0x20, 0x0000_0001))]);
        assert!(matches!(find_smbus_base(Some(ALLOCATION), Some(&NullMutex)), Err(SmbusError::Unavailable(_))));
    }

    #[test]
    fn an_absent_device_is_refused() {
        install_config_space(&[]);
        let refused = find_smbus_base(Some(ALLOCATION), Some(&NullMutex));
        assert_eq!(
            refused,
            Err(SmbusError::Unavailable("No Intel SMBus controller found (00:1F.4 absent; 00:1F.3 absent)".into()))
        );
    }

    // --- ReadOnlyContractTest: the module must not be able to write to a
    // device on the bus.

    #[test]
    fn the_write_allowlist_is_one_selector_on_pmic_addresses() {
        // The whole permitted set for write_byte. The SPD hubs are written
        // too, but not through here -- select_spd_page is the only path to
        // them, and it cannot be pointed at a register of the caller's
        // choosing. Anything beyond the ADC channel selector appearing here
        // means a widening that was not reviewed.
        let expected: Vec<(u8, u8)> = PMIC_ADDRESSES.iter().map(|a| (*a, 0x30)).collect();
        assert_eq!(WRITE_ALLOWLIST.to_vec(), expected);
    }

    #[test]
    fn the_only_writable_pmic_register_is_the_adc_selector() {
        for (address, register) in WRITE_ALLOWLIST {
            if PMIC_ADDRESSES.contains(&address) {
                assert_eq!(register, PMIC_TELEMETRY_SELECT_REGISTER);
            }
        }
    }

    #[test]
    fn no_rail_control_register_is_writable() {
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for address in PMIC_ADDRESSES {
            for register in PMIC_RAIL_CONTROL_REGISTERS {
                assert!(!WRITE_ALLOWLIST.contains(&(address, register)));
                assert!(matches!(reader.write_byte(address, register, 0x00, 0x00), Err(SmbusError::Refused(_))));
            }
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn no_spd_hub_register_is_reachable_through_write_byte() {
        // Including the page register: the hubs are not in the allowlist at
        // all, so the general write path cannot touch them.
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for address in SPD_HUB_ADDRESSES {
            for register in [0x00, 0x0A, 0x0B, 0x0C, 0x30, 0x80, 0x8B] {
                assert!(matches!(reader.write_byte(address, register, 0x00, 0x00), Err(SmbusError::Refused(_))));
            }
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn the_page_selector_cannot_be_aimed_at_another_register() {
        // The register is not a parameter. That is the whole safety property:
        // no caller, and no bug in a caller, can steer this at the EEPROM
        // window where a write would corrupt the module's SPD.
        let io = fake_io(None, (0, 0));
        make_reader().select_spd_page(0x50, 0x04).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_COMMAND), vec![SPD_HUB_PAGE_REGISTER]);
    }

    #[test]
    fn the_page_selector_refuses_a_non_hub_address() {
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for address in [0x00, 0x18, 0x48, 0x4A, 0x58, 0x77] {
            assert!(matches!(reader.select_spd_page(address, 0x04), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn the_page_value_is_masked_to_the_three_page_bits() {
        // A caller passing a byte-wide value must not be able to set anything
        // else in MR11 -- the addressing-mode bits live in the same register.
        let io = fake_io(None, (0, 0));
        make_reader().select_spd_page(0x50, 0xFF).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_DATA0), vec![SPD_HUB_PAGE_MASK]);
    }

    #[test]
    fn the_page_select_is_a_process_call_with_the_direction_bit_set() {
        // Both halves matter and were measured on the bench: a byte write is
        // refused outright while the platform's SPD Write Disable is armed,
        // and the interlock classifies on the direction bit, so the bit has to
        // be set for the write phase to reach the hub. This is the transaction
        // CPU-Z issues for the same purpose.
        let io = fake_io(None, (0, 0));
        make_reader().select_spd_page(0x50, 0x04).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_CONTROL), vec![PROTOCOL_PROC_CALL | CONTROL_START]);
        assert_eq!(values_at(&io, BASE + REG_HOST_ADDRESS), vec![(0x50 << 1) | 1]);
        // The second data byte is what would land in the write-protection
        // register if the hub consumed it; it is always zero.
        assert_eq!(values_at(&io, BASE + REG_HOST_DATA1), vec![0x00]);
    }

    #[test]
    fn read_spd_restores_the_page_the_hub_was_left_on() {
        // The hub reports it is currently on page 3.
        let io = fake_io(Some(vec![]), (0x03, 0x00));
        make_reader().read_spd(0x50, 0x200, 8, 0x00).unwrap();
        let page_writes = values_at(&io, BASE + REG_HOST_DATA0);
        assert_eq!(page_writes[0], 0x04);
        // Page 4 holds 0x200; the hub is put back on 3 afterwards.
        assert_eq!(page_writes, vec![0x04, 0x03]);
    }

    #[test]
    fn read_spd_holds_the_bus_for_the_whole_sequence() {
        // The EEPROM window returns bytes from whichever page an earlier
        // transaction selected, so the page select and the reads that depend
        // on it have to be one critical section. Taking the mutex per byte
        // lets another master repage the hub mid-read, which on the bench
        // returned a part number ending "Kz>>>}" -- right offsets, wrong page.
        #[derive(Default)]
        struct Counts {
            acquisitions: AtomicUsize,
            depth: AtomicUsize,
            max_depth: AtomicUsize,
        }
        struct CountingMutex(Arc<Counts>);
        struct Release(Arc<Counts>);
        impl Drop for Release {
            fn drop(&mut self) {
                self.0.depth.fetch_sub(1, Ordering::SeqCst);
            }
        }
        impl BusMutex for CountingMutex {
            fn acquire(&self) -> Result<BusGuard, SmbusError> {
                self.0.acquisitions.fetch_add(1, Ordering::SeqCst);
                let depth = self.0.depth.fetch_add(1, Ordering::SeqCst) + 1;
                self.0.max_depth.fetch_max(depth, Ordering::SeqCst);
                Ok(Box::new(Release(self.0.clone())))
            }
        }

        let counts = Arc::new(Counts::default());
        fake_io(Some(vec![]), (0, 0));
        let reader = PchSmbusReader::with_options(SmbusOptions {
            mutex: Some(Box::new(CountingMutex(counts.clone()))),
            base: Some(BASE),
            sleep: Some(Box::new(|_| {})),
            ..SmbusOptions::default()
        })
        .unwrap();
        reader.read_spd(0x50, 0x200, 48, 0x00).unwrap();
        assert_eq!(counts.acquisitions.load(Ordering::SeqCst), 1);
        assert_eq!(counts.max_depth.load(Ordering::SeqCst), 1, "the mutex was re-entered");
    }

    #[test]
    fn read_spd_refuses_an_address_that_is_not_an_spd_hub() {
        let io = fake_io(None, (0, 0));
        assert!(matches!(make_reader().read_spd(0x48, 0x200, 8, 0x00), Err(SmbusError::Refused(_))));
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn the_allowlisted_write_drives_the_bus_with_direction_clear() {
        let io = fake_io(None, (0, 0));
        make_reader().write_byte(0x48, 0x30, 0x80, 0x00).unwrap();
        let slave = first_at(&io, BASE + REG_HOST_ADDRESS);
        assert_eq!(slave & 0x01, 0x00);
        assert_eq!(slave >> 1, 0x48);
        assert_eq!(first_at(&io, BASE + REG_HOST_DATA0), 0x80);
    }

    #[test]
    fn a_write_leaves_the_controller_clean() {
        let io = fake_io(None, (0, 0));
        make_reader().write_byte(0x48, 0x30, 0x80, 0x00).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_STATUS).last(), Some(&STATUS_CLEAR_MASK));
    }

    #[test]
    fn reads_never_clear_the_direction_bit() {
        // Reads and the write are separate primitives; no argument to a read
        // can turn it into a write.
        let calls: [&dyn Fn(&PchSmbusReader); 3] = [
            &|r| {
                r.read_word_bytes(0x19, 0x05).unwrap();
            },
            &|r| {
                r.read_byte(0x48, 0x21, 0x00).unwrap();
            },
            &|r| {
                r.read_byte(0x50, 0x31, 0x00).unwrap();
            },
        ];
        for call in calls {
            let io = fake_io(None, (0xC1, 0xE8));
            call(&make_reader());
            assert_eq!(first_at(&io, BASE + REG_HOST_ADDRESS) & 0x01, 0x01);
        }
    }

    #[test]
    fn no_write_word_primitive_exists() {
        let source = include_str!("intel_pch_smbus.rs");
        let code = source.split(concat!("#[cfg", "(test)]")).next().unwrap();
        for forbidden in [concat!("PROTOCOL_BYTE", "_WRITE"), concat!("write", "_word")] {
            assert!(!code.contains(forbidden), "{forbidden}");
        }
    }

    #[test]
    fn the_direction_bit_is_always_read() {
        let io = fake_io(None, (0xC1, 0xE8));
        make_reader().read_word_bytes(0x19, 0x05).unwrap();
        let slave = first_at(&io, BASE + REG_HOST_ADDRESS);
        assert_eq!(slave & 0x01, 0x01);
        assert_eq!(slave >> 1, 0x19);
    }

    #[test]
    fn spd_eeprom_addresses_are_unreachable() {
        let io = fake_io(None, (0, 0));
        let reader = make_reader();
        for address in 0x50..0x58 {
            assert!(matches!(reader.read_word_bytes(address, 0x00), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn only_thermal_sensor_addresses_are_allowed() {
        assert_eq!(ALLOWED_ADDRESSES.to_vec(), (0x18..0x20).collect::<Vec<u8>>());
    }

    #[test]
    fn only_controller_registers_are_written() {
        let io = fake_io(None, (0xC1, 0xE8));
        make_reader().read_word_bytes(0x19, 0x05).unwrap();
        let written: std::collections::BTreeSet<u16> = writes(&io).into_iter().map(|(p, _)| p - BASE).collect();
        assert_eq!(
            written,
            [REG_HOST_STATUS, REG_HOST_ADDRESS, REG_HOST_COMMAND, REG_HOST_CONTROL].into_iter().collect()
        );
    }

    // --- TransactionTest

    #[test]
    fn a_word_read_issues_the_word_protocol_and_start() {
        let io = fake_io(None, (0xC1, 0xE8));
        make_reader().read_word_bytes(0x19, 0x05).unwrap();
        assert_eq!(first_at(&io, BASE + REG_HOST_CONTROL), PROTOCOL_WORD_DATA | CONTROL_START);
    }

    #[test]
    fn the_register_is_sent_as_the_command_byte() {
        let io = fake_io(None, (0xC1, 0xE8));
        make_reader().read_word_bytes(0x19, 0x05).unwrap();
        assert_eq!(first_at(&io, BASE + REG_HOST_COMMAND), 0x05);
    }

    #[test]
    fn bytes_come_back_in_wire_order() {
        fake_io(None, (0xC1, 0xE8));
        assert_eq!(make_reader().read_word_bytes(0x19, 0x05), Ok((0xC1, 0xE8)));
    }

    #[test]
    fn status_is_cleared_before_and_after() {
        let io = fake_io(None, (0xC1, 0xE8));
        make_reader().read_word_bytes(0x19, 0x05).unwrap();
        assert_eq!(values_at(&io, BASE + REG_HOST_STATUS), vec![STATUS_CLEAR_MASK, STATUS_CLEAR_MASK]);
    }

    #[test]
    fn a_bus_error_raises_and_still_clears_status() {
        let io = fake_io(Some(vec![0x00, STATUS_DEV_ERR]), (0, 0));
        assert!(matches!(make_reader().read_word_bytes(0x19, 0x05), Err(SmbusError::Bus(_))));
        assert_eq!(writes(&io).last(), Some(&(BASE + REG_HOST_STATUS, STATUS_CLEAR_MASK)));
    }

    #[test]
    fn a_stuck_bus_times_out_rather_than_hanging() {
        fake_io(Some(vec![0x01; 4000]), (0, 0));
        let ticks = Mutex::new(VecDeque::from([0.0]));
        let reader = PchSmbusReader::with_options(SmbusOptions {
            mutex: Some(Box::new(NullMutex)),
            base: Some(BASE),
            monotonic: Some(Box::new(move || ticks.lock().unwrap().pop_front().unwrap_or(1.0))),
            sleep: Some(Box::new(|_| {})),
            ..SmbusOptions::default()
        })
        .unwrap();
        assert!(matches!(reader.read_word_bytes(0x19, 0x05), Err(SmbusError::Timeout(_))));
    }

    #[test]
    fn an_out_of_range_timeout_is_refused() {
        fake_io(None, (0, 0));
        for bad in [0.0, -1.0, 0.5] {
            let built = PchSmbusReader::with_options(SmbusOptions {
                mutex: Some(Box::new(NullMutex)),
                base: Some(BASE),
                timeout: bad,
                ..SmbusOptions::default()
            });
            assert!(matches!(built, Err(SmbusError::Refused(_))), "{bad}");
        }
    }

    // --- ScanTest

    #[test]
    fn only_answering_addresses_are_returned() {
        let answering = [0x19u8, 0x1B];
        FakeHardware::new()
            .with_port_reader(move |fake, port, _width| {
                if port == BASE + REG_HOST_STATUS {
                    let current = fake.port_value(BASE + REG_HOST_ADDRESS).map(|v| (v >> 1) as u8);
                    if current.is_some_and(|c| answering.contains(&c)) {
                        return Some(STATUS_INTR as u32);
                    }
                    return Some(STATUS_DEV_ERR as u32);
                }
                Some(0x00)
            })
            .install();
        assert_eq!(make_reader().responding_addresses(0x05), vec![0x19, 0x1B]);
    }
}
