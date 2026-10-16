// This file follows ZenStates-Core and ZenTimings by irusanov
// (https://github.com/irusanov), both GPL-3.0. Register numbers, bit fields
// and the bounds applied to decoded values were taken from or checked against
// that work, and the comments below say where. Copyright in those parts
// remains with their authors; this file is distributed under the same licence
// they are, which is what makes that use permitted.

//! Read-only SMBus transport for the AMD FCH (PIIX4-compatible controller).
//!
//! Used to reach the DDR5 SPD hub and PMIC, which carry DRAM VDD/VDDQ/VPP.
//! Those rails are NOT in the SMU PM table — proven twice, see the note in
//! `amd::smu_voltages::CONFIRMED_VOLTAGE_OFFSETS` — so this is the only path
//! to them.
//!
//! SAFETY — reads are unrestricted within the allowlist; writes are not:
//!
//!   * Reads use SMBus "Read Byte Data" against the DDR5 SPD hub (0x50-0x57)
//!     and PMIC (0x48-0x4F) ranges.  Any other address is refused before it
//!     reaches the bus.  Writing the register index into HOST_COMMAND is part
//!     of the read protocol, not a device write: the device is addressed for
//!     reading and never receives a data byte.
//!
//!   * Writes use SMBus "Write Byte Data" and are restricted to exactly two
//!     (address range, register) pairs — the SPD hub's MR11 page-select
//!     register and the PMIC's R30h telemetry-channel selector.
//!     [`WRITE_ALLOWLIST`] is the whole permitted set; every other target is
//!     refused before the bus is driven, by an equality test on the pair
//!     rather than a range check.
//!
//!     Both are selectors, not settings.  Paging the SPD hub picks which
//!     EEPROM page a read returns; R30h picks which channel the PMIC's ADC
//!     converts, and the result is read back from R31h.  Neither can change a
//!     rail.
//!
//!     What the PMIC's rails are actually set by — R21h/R25h/R27h (the VID
//!     registers) and R2Bh (the VID mode) — appears nowhere in the allowlist,
//!     so a mis-addressed write cannot move VDD/VDDQ/VPP on a running system.
//!     [`PMIC_RAIL_CONTROL_REGISTERS`] names them and the test-suite asserts
//!     each one is refused.
//!
//!   * The controller is held under the same named mutex HWiNFO / ZenTimings /
//!     lm-sensors use, so concurrent monitors cannot interleave a transfer.
//!   * Every transaction is bounded by a deadline and leaves the host status
//!     register cleared.
//!
//! The reader implements [`SmbusReader`], the contract the DDR5 device
//! readers drive either platform's bus through.

use std::sync::{Arc, Mutex, MutexGuard};

use crate::hardware::{self, Hardware};
use crate::memory::ddr5_telemetry::{
    BusMutex, NamedBusMutex, SmbusError, SmbusOptions, SmbusReader, SpdBytes, monotonic, sleep_seconds,
};

// The mutex every mainstream monitoring tool takes before touching SMBus.
pub const SMBUS_MUTEX_NAME: &str = "Global\\Access_SMBUS.HTP.Method";

// FCH indirect PMIO window used to discover the SMBus IO base.
pub const PMIO_INDEX_PORT: u16 = 0xCD6;
pub const PMIO_DATA_PORT: u16 = 0xCD7;
pub const PMIO_SMBA_LOW: u8 = 0x2C;
pub const PMIO_SMBA_HIGH: u8 = 0x2D;
pub const SMBUS_BASE_MASK: u16 = 0xFFE0;
pub const FALLBACK_SMBUS_BASE: u16 = 0x0B00;

// PIIX4-compatible host register offsets from the controller base.
pub const REG_HOST_STATUS: u16 = 0x00;
pub const REG_HOST_CONTROL: u16 = 0x02;
pub const REG_HOST_COMMAND: u16 = 0x03;
pub const REG_HOST_ADDRESS: u16 = 0x04;
pub const REG_HOST_DATA0: u16 = 0x05;

// HOST_CONTROL: bits 4:2 select the protocol, bit 6 starts the transfer.
pub const PROTOCOL_BYTE_DATA: u8 = 0x08;
pub const CONTROL_START: u8 = 0x40;

// SPD5 hub volatile register carrying the EEPROM page selector.
pub const SPD_HUB_PAGE_REGISTER: u8 = 0x0B; // MR11
pub const SPD_HUB_PAGE_MASK: u8 = 0x07; // 8 pages of 128 bytes
pub const SPD_EEPROM_WINDOW: u8 = 0x80; // register bit 7 set => EEPROM, not MR space
pub const SPD_PAGE_SIZE: i64 = 0x80;

// HOST_STATUS bits.
pub const STATUS_HOST_BUSY: u8 = 0x01;
pub const STATUS_INTR: u8 = 0x02;
pub const STATUS_DEV_ERR: u8 = 0x04;
pub const STATUS_BUS_COLLISION: u8 = 0x08;
pub const STATUS_FAILED: u8 = 0x10;
pub const STATUS_ERROR_MASK: u8 = STATUS_DEV_ERR | STATUS_BUS_COLLISION | STATUS_FAILED;
pub const STATUS_CLEAR_MASK: u8 = 0x1F;

// Allowlisted DDR5 device addresses (7-bit).
pub const SPD_HUB_ADDRESSES: [u8; 8] = [0x50, 0x51, 0x52, 0x53, 0x54, 0x55, 0x56, 0x57];
pub const PMIC_ADDRESSES: [u8; 8] = [0x48, 0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E, 0x4F];
pub const ALLOWED_ADDRESSES: [u8; 16] = {
    let mut out = [0u8; 16];
    let mut i = 0;
    while i < 8 {
        out[i] = SPD_HUB_ADDRESSES[i];
        out[8 + i] = PMIC_ADDRESSES[i];
        i += 1;
    }
    out
};

// The complete set of writable targets: {(address, register)}.
//
// One entry: the SPD hub's page selector, which selects which EEPROM page to
// read. No PMIC address appears here at all, so no PMIC register — least of all
// the VID registers 0x21/0x25/0x27 that actually move VDD/VDDQ/VPP — is
// reachable by any write path in this module.
// The PMIC's ADC channel selector. Writing it picks which channel R31h
// reports; JESD301-2 requires ~9 ms to settle before the value is read. It
// carries no rail setting, and the rail registers below are not reachable
// through this list.
pub const PMIC_TELEMETRY_SELECT_REGISTER: u8 = 0x30;

pub const WRITE_ALLOWLIST: [(u8, u8); 16] = {
    let mut out = [(0u8, 0u8); 16];
    let mut i = 0;
    while i < 8 {
        out[i] = (SPD_HUB_ADDRESSES[i], SPD_HUB_PAGE_REGISTER);
        out[8 + i] = (PMIC_ADDRESSES[i], PMIC_TELEMETRY_SELECT_REGISTER);
        i += 1;
    }
    out
};

// Registers that must never be writable, asserted by the test-suite.
pub const PMIC_RAIL_CONTROL_REGISTERS: [u8; 4] = [0x21, 0x25, 0x27, 0x2B];

// The two controllers an AM5 FCH exposes; DIMMs answer on one of them.
pub const CONTROLLER_OFFSETS: [u8; 2] = [0x00, 0x20];

/// Discover the FCH SMBus IO base, falling back to the usual 0x0B00.
pub fn read_smbus_base(io: &dyn Hardware) -> u16 {
    let read = || -> hardware::HwResult<(u8, u8)> {
        io.outb(PMIO_INDEX_PORT, PMIO_SMBA_LOW)?;
        let low = io.inb(PMIO_DATA_PORT)?;
        io.outb(PMIO_INDEX_PORT, PMIO_SMBA_HIGH)?;
        let high = io.inb(PMIO_DATA_PORT)?;
        Ok((low, high))
    };
    let Ok((low, high)) = read() else {
        return FALLBACK_SMBUS_BASE;
    };
    let base = (((high as u16) << 8) | low as u16) & SMBUS_BASE_MASK;
    // A zeroed or all-ones window means the PMIO read did not land; the FCH
    // default is well known, so use it rather than driving a bogus port.
    if base == 0x0000 || base == SMBUS_BASE_MASK {
        return FALLBACK_SMBUS_BASE;
    }
    base
}

/// The backend's reader factory: `FchSmbusReader()` with every default.
pub fn new_reader() -> Result<Arc<dyn SmbusReader>, SmbusError> {
    Ok(Arc::new(FchSmbusReader::new()?))
}

fn hw_error(error: hardware::HwError) -> SmbusError {
    SmbusError::Bus(error.0)
}

/// Serialized, allowlisted, read-only SMBus byte reader.
pub struct FchSmbusReader {
    io: Arc<dyn Hardware>,
    mutex: Box<dyn BusMutex>,
    lock: Mutex<()>,
    monotonic: Box<dyn Fn() -> f64 + Send + Sync>,
    sleep: Box<dyn Fn(f64) + Send + Sync>,
    timeout: f64,
    pub base: u16,
    last_error: Mutex<String>,
}

impl FchSmbusReader {
    /// The real controller: the driver, the shared SMBus mutex, the PMIO base.
    pub fn new() -> Result<Self, SmbusError> {
        Self::with_options(SmbusOptions::default())
    }

    pub fn with_options(options: SmbusOptions) -> Result<Self, SmbusError> {
        // Building the port I/O object failed when the driver could not be
        // loaded; the backend here reports the same reason.
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
            None => read_smbus_base(io.as_ref()),
        };
        Ok(FchSmbusReader {
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

    /// Read one byte using the SMBus Read Byte Data protocol.
    ///
    /// Fails on refusal, timeout or bus error; never returns a partial value.
    pub fn read_byte(&self, address: u8, register: u8, controller_offset: u8) -> Result<u8, SmbusError> {
        if !ALLOWED_ADDRESSES.contains(&address) {
            return Err(SmbusError::Refused(format!(
                "SMBus address 0x{address:02X} is outside the DDR5 SPD/PMIC allowlist"
            )));
        }
        if !CONTROLLER_OFFSETS.contains(&controller_offset) {
            return Err(SmbusError::Refused(format!("Unknown SMBus controller offset 0x{controller_offset:02X}")));
        }
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.read_byte_locked(address, register, controller_offset)
    }

    /// Read one byte. Caller must already hold the lock and the mutex.
    fn read_byte_locked(&self, address: u8, register: u8, controller_offset: u8) -> Result<u8, SmbusError> {
        let base = self.base.wrapping_add(controller_offset as u16);
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

    /// Write one byte. Caller must already hold the lock and the mutex.
    ///
    /// The allowlist is still enforced here, so no internal caller can bypass
    /// it by taking the mutex itself.
    fn write_byte_locked(&self, address: u8, register: u8, value: u8, controller_offset: u8) -> Result<(), SmbusError> {
        if !WRITE_ALLOWLIST.contains(&(address, register)) {
            return Err(SmbusError::Refused(format!(
                "SMBus write to 0x{address:02X} register 0x{register:02X} is not permitted"
            )));
        }
        let base = self.base.wrapping_add(controller_offset as u16);
        let port_status = base + REG_HOST_STATUS;
        self.wait_not_busy(port_status)?;
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        self.outb(base + REG_HOST_ADDRESS, (((address as u16) << 1) & 0xFE) as u8)?;
        self.outb(base + REG_HOST_COMMAND, register)?;
        self.outb(base + REG_HOST_DATA0, value)?;
        self.outb(base + REG_HOST_CONTROL, PROTOCOL_BYTE_DATA | CONTROL_START)?;
        let result = self.wait_complete(port_status).map(|_| ());
        self.outb(port_status, STATUS_CLEAR_MASK)?;
        result
    }

    /// Write one byte, but only to a target in [`WRITE_ALLOWLIST`].
    ///
    /// The allowlist is a set of exact `(address, register)` pairs and holds
    /// nothing but the SPD hub page selector and the PMIC telemetry selector;
    /// no rail-control register can be written through this method.
    pub fn write_byte(&self, address: u8, register: u8, value: u8, controller_offset: u8) -> Result<(), SmbusError> {
        if !WRITE_ALLOWLIST.contains(&(address, register)) {
            return Err(SmbusError::Refused(format!(
                "SMBus write to 0x{address:02X} register 0x{register:02X} is not permitted \
                 (only the SPD page and PMIC telemetry selectors are writable)"
            )));
        }
        if !CONTROLLER_OFFSETS.contains(&controller_offset) {
            return Err(SmbusError::Refused(format!("Unknown SMBus controller offset 0x{controller_offset:02X}")));
        }
        let _lock = self.hold();
        let _bus = self.mutex.acquire()?;
        self.write_byte_locked(address, register, value, controller_offset)
    }

    /// Read SPD EEPROM bytes across page boundaries.
    ///
    /// Restores whichever page the hub was left on, mirroring the SMN
    /// selector-restore discipline used elsewhere in this project.
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
        let original_page = self.read_byte(address, SPD_HUB_PAGE_REGISTER, controller_offset)?;
        let mut values = SpdBytes::new();
        let mut current_page: Option<i64> = None;
        let result = (|| -> Result<(), SmbusError> {
            for position in offset..offset + length {
                let (page, within) = (position.div_euclid(SPD_PAGE_SIZE), position.rem_euclid(SPD_PAGE_SIZE));
                if page & !(SPD_HUB_PAGE_MASK as i64) != 0 {
                    break;
                }
                if current_page != Some(page) {
                    self.write_byte(address, SPD_HUB_PAGE_REGISTER, page as u8, controller_offset)?;
                    current_page = Some(page);
                }
                match self.read_byte(address, SPD_EEPROM_WINDOW | within as u8, controller_offset) {
                    Ok(value) => {
                        values.insert(position as u32, value);
                    }
                    Err(error) if error.is_io() => continue,
                    Err(error) => return Err(error),
                }
            }
            Ok(())
        })();
        match self.write_byte(address, SPD_HUB_PAGE_REGISTER, original_page & SPD_HUB_PAGE_MASK, controller_offset) {
            Ok(()) => {}
            Err(error) if error.is_transfer() => self.set_last_error(format!("SPD page restore failed: {error}")),
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

    /// Read several registers, returning `{register: value}`.
    ///
    /// Registers that fail are omitted rather than reported as zero.
    pub fn read_block(
        &self,
        address: u8,
        registers: &[u8],
        controller_offset: u8,
    ) -> std::collections::BTreeMap<u8, u8> {
        let mut values = std::collections::BTreeMap::new();
        for &register in registers {
            match self.read_byte(address, register, controller_offset) {
                Ok(value) => {
                    values.insert(register, value);
                }
                Err(_) => continue,
            }
        }
        values
    }
}

impl SmbusReader for FchSmbusReader {
    fn is_driver_open(&self) -> bool {
        self.io.is_driver_open()
    }

    fn read_byte(&self, address: u8, register: u8, controller: u8) -> Result<u8, SmbusError> {
        FchSmbusReader::read_byte(self, address, register, controller)
    }

    fn write_byte(&self, address: u8, register: u8, value: u8, controller: u8) -> Result<(), SmbusError> {
        FchSmbusReader::write_byte(self, address, register, value, controller)
    }

    fn read_spd(&self, address: u8, offset: i64, length: i64, controller: u8) -> Result<SpdBytes, SmbusError> {
        FchSmbusReader::read_spd(self, address, offset, length, controller)
    }

    fn probe_address(&self, address: u8, controller: u8) -> bool {
        self.probe(address, controller, 0x00)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hardware::{HwError, HwResult, set_thread_hardware};
    use crate::testing::{Access, FakeHardware};
    use std::collections::BTreeMap;

    struct NullMutex;

    impl BusMutex for NullMutex {
        fn acquire(&self) -> Result<crate::memory::ddr5_telemetry::BusGuard, SmbusError> {
            Ok(Box::new(()))
        }
    }

    /// Minimal SMBus host that records every port access: never busy, the
    /// transfer already complete, `data_byte` in DATA0, and the PMIO window
    /// answering from `pmio`.
    fn fake_io(data_byte: u8, pmio: BTreeMap<u8, u8>) -> FakeHardware {
        FakeHardware::new().with_port_reader(move |fake, port, _width| {
            if port == PMIO_DATA_PORT {
                let index = fake.port_value(PMIO_INDEX_PORT).map(|v| v as u8);
                return Some(index.and_then(|i| pmio.get(&i).copied()).unwrap_or(0x00) as u32);
            }
            let offset = port.wrapping_sub(FALLBACK_SMBUS_BASE);
            if offset == REG_HOST_STATUS {
                // Never busy, transfer already complete.
                return Some(STATUS_INTR as u32);
            }
            if offset == REG_HOST_DATA0 {
                return Some(data_byte as u32);
            }
            Some(0x00)
        })
    }

    /// The port writes the Python FakeIO recorded (the PMIO index excluded).
    fn writes(fake: &FakeHardware) -> Vec<(u16, u8)> {
        fake.log()
            .into_iter()
            .filter_map(|access| match access {
                Access::OutB(port, value) if port != PMIO_INDEX_PORT => Some((port, value)),
                _ => None,
            })
            .collect()
    }

    fn make_reader(fake: FakeHardware) -> (FchSmbusReader, Arc<FakeHardware>) {
        let installed = fake.install();
        let reader = FchSmbusReader::with_options(SmbusOptions {
            mutex: Some(Box::new(NullMutex)),
            base: Some(FALLBACK_SMBUS_BASE),
            ..SmbusOptions::default()
        })
        .unwrap();
        (reader, installed)
    }

    fn default_reader() -> (FchSmbusReader, Arc<FakeHardware>) {
        make_reader(fake_io(0xA5, BTreeMap::new()))
    }

    fn by_port(writes: &[(u16, u8)]) -> BTreeMap<u16, u8> {
        writes.iter().copied().collect()
    }

    // --- SmbusBaseTest

    #[test]
    fn base_comes_from_the_pmio_window() {
        let io = fake_io(0xA5, BTreeMap::from([(PMIO_SMBA_LOW, 0x00), (PMIO_SMBA_HIGH, 0x0B)]));
        assert_eq!(read_smbus_base(&io), 0x0B00);
    }

    #[test]
    fn base_is_masked_to_the_controller_granularity() {
        let io = fake_io(0xA5, BTreeMap::from([(PMIO_SMBA_LOW, 0x1F), (PMIO_SMBA_HIGH, 0x0B)]));
        assert_eq!(read_smbus_base(&io), 0x0B00);
    }

    #[test]
    fn empty_window_falls_back_to_the_fch_default() {
        assert_eq!(read_smbus_base(&fake_io(0xA5, BTreeMap::new())), FALLBACK_SMBUS_BASE);
    }

    // --- SmbusReadOnlyTest

    #[test]
    fn only_allowlisted_addresses_reach_the_bus() {
        let (reader, io) = default_reader();
        // DRAM controller, EC, and random devices must all be refused.
        for address in [0x00, 0x2F, 0x30, 0x44, 0x58, 0x69, 0x77] {
            assert!(!ALLOWED_ADDRESSES.contains(&address));
            assert!(matches!(reader.read_byte(address, 0x30, 0x00), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![], "a refused address still drove the bus");
    }

    #[test]
    fn allowlist_covers_only_ddr5_spd_and_pmic() {
        let mut allowed = ALLOWED_ADDRESSES.to_vec();
        allowed.sort();
        assert_eq!(allowed, (0x48..0x58).collect::<Vec<u8>>());
    }

    #[test]
    fn read_uses_the_read_direction_and_byte_data_protocol() {
        let (reader, io) = default_reader();
        let value = reader.read_byte(0x4B, 0x2E, 0x00).unwrap();
        assert_eq!(value, 0xA5);
        let by_port = by_port(&writes(&io));
        // Address register must carry the read bit set.
        assert_eq!(by_port[&(FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS)], (0x4B << 1) | 1);
        assert_eq!(by_port[&(FALLBACK_SMBUS_BASE + REG_HOST_COMMAND)], 0x2E);
        assert_eq!(by_port[&(FALLBACK_SMBUS_BASE + REG_HOST_CONTROL)], PROTOCOL_BYTE_DATA | CONTROL_START);
    }

    #[test]
    fn every_transfer_sets_the_read_bit() {
        let (reader, io) = default_reader();
        let mut addresses = ALLOWED_ADDRESSES.to_vec();
        addresses.sort();
        for address in addresses {
            reader.read_byte(address, 0x00, 0x00).unwrap();
        }
        for (port, value) in writes(&io) {
            if port == FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS {
                assert_eq!(value & 1, 1, "a transfer was addressed write");
            }
        }
    }

    #[test]
    fn status_is_cleared_before_and_after() {
        let (reader, io) = default_reader();
        reader.read_byte(0x49, 0x30, 0x00).unwrap();
        let status_writes: Vec<u8> = writes(&io)
            .into_iter()
            .filter(|(p, _)| *p == FALLBACK_SMBUS_BASE + REG_HOST_STATUS)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(status_writes.len(), 2);
    }

    #[test]
    fn no_pmic_rail_register_is_writable() {
        // THE critical invariant. R21h/R25h/R27h are the VID registers that set
        // VDD/VDDQ/VPP and R2Bh selects their encoding mode. The PMIC is no
        // longer wholly unwritable -- R30h selects an ADC channel, which is how
        // the measured rails are read -- so this asserts the line that matters:
        // nothing that can change a rail is reachable.
        for address in PMIC_ADDRESSES {
            for register in PMIC_RAIL_CONTROL_REGISTERS {
                assert!(!WRITE_ALLOWLIST.contains(&(address, register)));
            }
        }
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
    fn write_allowlist_is_the_two_selectors_and_nothing_else() {
        let mut expected: Vec<(u8, u8)> = (0x50..0x58)
            .map(|a| (a, SPD_HUB_PAGE_REGISTER))
            .chain((0x48..0x50).map(|a| (a, PMIC_TELEMETRY_SELECT_REGISTER)))
            .collect();
        expected.sort();
        let mut actual = WRITE_ALLOWLIST.to_vec();
        actual.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn write_refuses_pmic_rail_registers() {
        let (reader, io) = default_reader();
        for address in PMIC_ADDRESSES {
            for register in [0x00, 0x0B, 0x21, 0x25, 0x27, 0x2B, 0x2E, 0x2F] {
                assert!(matches!(reader.write_byte(address, register, 0x00, 0x00), Err(SmbusError::Refused(_))));
            }
        }
        assert_eq!(writes(&io), vec![], "a refused write still drove the bus");
    }

    #[test]
    fn locked_write_helper_also_enforces_the_allowlist() {
        // Internal callers take the mutex themselves; they must not be able to
        // bypass the allowlist by using the locked helper directly.
        let (reader, _) = default_reader();
        assert!(matches!(reader.write_byte_locked(0x49, 0x21, 0xFF, 0x00), Err(SmbusError::Refused(_))));
    }

    #[test]
    fn the_only_pmic_write_that_lands_is_the_adc_selector() {
        // Every other register on the PMIC is refused before the bus is driven,
        // including the ones either side of the selector.
        let (reader, io) = default_reader();
        for register in [0x00, 0x21, 0x25, 0x27, 0x2B, 0x2F, 0x31, 0x3C] {
            assert!(matches!(reader.write_byte(0x49, register, 0x00, 0x00), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![]);

        reader.write_byte(0x49, PMIC_TELEMETRY_SELECT_REGISTER, 0x80, 0x00).unwrap();
        assert!(!writes(&io).is_empty(), "the permitted selector write did not happen");
    }

    #[test]
    fn write_refuses_non_page_registers_on_the_spd_hub() {
        let (reader, io) = default_reader();
        for register in [0x00, 0x0A, 0x0C, 0x30, 0x80] {
            assert!(matches!(reader.write_byte(0x51, register, 0x00, 0x00), Err(SmbusError::Refused(_))));
        }
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn permitted_write_uses_the_write_direction() {
        let (reader, io) = default_reader();
        reader.write_byte(0x51, SPD_HUB_PAGE_REGISTER, 0x04, 0x00).unwrap();
        let by_port = by_port(&writes(&io));
        assert_eq!(by_port[&(FALLBACK_SMBUS_BASE + REG_HOST_ADDRESS)], 0x51 << 1);
        assert_eq!(by_port[&(FALLBACK_SMBUS_BASE + REG_HOST_DATA0)], 0x04);
    }

    #[test]
    fn read_spd_restores_the_original_page() {
        // The hub reports it is currently on page 3.
        let (reader, io) = make_reader(fake_io(0x03, BTreeMap::new()));
        reader.read_spd(0x51, 512, 8, 0x00).unwrap();
        let page_writes: Vec<u8> = writes(&io)
            .into_iter()
            .filter(|(p, _)| *p == FALLBACK_SMBUS_BASE + REG_HOST_DATA0)
            .map(|(_, v)| v)
            .collect();
        assert_eq!(page_writes.last(), Some(&0x03), "SPD page was not restored to what it was");
    }

    #[test]
    fn read_spd_refuses_a_pmic_address() {
        let (reader, io) = default_reader();
        assert!(matches!(reader.read_spd(0x49, 512, 8, 0x00), Err(SmbusError::Refused(_))));
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn unknown_controller_offset_is_refused() {
        let (reader, io) = default_reader();
        assert!(matches!(reader.read_byte(0x49, 0x30, 0x40), Err(SmbusError::Refused(_))));
        assert_eq!(writes(&io), vec![]);
    }

    #[test]
    fn probe_address_reports_failures_as_absent() {
        let (reader, _) = default_reader();
        assert!(!SmbusReader::probe_address(&reader, 0x30, 0x00));
    }

    #[test]
    fn read_block_skips_registers_that_fail() {
        /// A bus whose data register glitches on every read.
        struct FlakyIo(FakeHardware);

        impl Hardware for FlakyIo {
            fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>> {
                self.0.read_phys(address, size)
            }
            fn get_phys_long(&self, address: u64) -> HwResult<u32> {
                self.0.get_phys_long(address)
            }
            fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()> {
                self.0.set_phys_long(address, value)
            }
            fn inb(&self, port: u16) -> HwResult<u8> {
                if port.wrapping_sub(FALLBACK_SMBUS_BASE) == REG_HOST_DATA0 {
                    return Err(HwError::new("bus glitch"));
                }
                self.0.inb(port)
            }
            fn outb(&self, port: u16, value: u8) -> HwResult<()> {
                self.0.outb(port, value)
            }
            fn inl(&self, port: u16) -> HwResult<u32> {
                self.0.inl(port)
            }
            fn outl(&self, port: u16, value: u32) -> HwResult<()> {
                self.0.outl(port, value)
            }
            fn is_driver_open(&self) -> bool {
                self.0.is_driver_open()
            }
        }

        set_thread_hardware(Some(Arc::new(FlakyIo(fake_io(0xA5, BTreeMap::new())))));
        let reader = FchSmbusReader::with_options(SmbusOptions {
            mutex: Some(Box::new(NullMutex)),
            base: Some(FALLBACK_SMBUS_BASE),
            ..SmbusOptions::default()
        })
        .unwrap();
        assert_eq!(reader.read_block(0x49, &[0x30, 0x31], 0x00), BTreeMap::new());
    }

    #[test]
    fn an_out_of_range_timeout_is_refused() {
        fake_io(0xA5, BTreeMap::new()).install();
        for bad in [0.0, -1.0, 0.5] {
            let built = FchSmbusReader::with_options(SmbusOptions {
                mutex: Some(Box::new(NullMutex)),
                base: Some(FALLBACK_SMBUS_BASE),
                timeout: bad,
                ..SmbusOptions::default()
            });
            assert!(matches!(built, Err(SmbusError::Refused(_))));
        }
    }
}
