//! AMD AM5 System Management Network (SMN) reader.
//!
//! SMN registers on AMD client platforms are reached through the legacy PCI
//! configuration mechanism on the root complex device 00:00.0:
//!
//! * write the 32-bit SMN address to config register 0x60 (the index)
//! * read the 32-bit result from config register 0x64 (the data)
//!
//! Config space itself is accessed through I/O ports 0xCF8 (address) and
//! 0xCFC (data). This module wraps that protocol with:
//!
//! * vendor gating (device 00:00.0 must report AMD, 0x1022)
//! * serialization through a process lock and a named Windows mutex so
//!   concurrent tools do not clobber the shared CF8/CFC window
//! * save/restore of the caller's prior CF8 value
//! * a driver-open pre-check via inpoutx64's IsInpOutDriverOpen
//!
//! There is deliberately **no** public SMN write API. The only config-space
//! write performed is the selector write to register 0x60 required to point
//! the data window at the register being read.
//!
//! Port I/O goes through [`crate::hardware::hw`], so the decode/transport
//! logic can be tested with a fake backend without ever touching hardware.

use std::any::Any;
use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::hardware::{HwError, HwResult, hw};

// -- PCI config mechanism constants -----------------------------------------
//
// The mutex and the port I/O live in hardware::lowlevel_io: every transport
// needs them, and two copies would mean two handles onto one DLL and two
// mutexes sharing a name, which is the opposite of what the name is for.
pub use crate::hardware::lowlevel_io::{
    AMD_VENDOR_ID, CF8_PORT, CFC_PORT, DEFAULT_MUTEX_NAME, DEFAULT_MUTEX_TIMEOUT_MS, NamedMutex,
    PCI_CONFIG_ENABLE, SMN_DATA_REG, SMN_INDEX_REG, VENDOR_REG,
};

// RSMU mailbox, addressed over SMN like everything else above.
//
// These live here rather than with the version probe that first used them
// because the viewer needs them too: amd::smu_clocks drives the same mailbox to
// read clocks. Importing them from the probe made the shipped viewer pull that
// whole research module in behind four numbers, which the packaging boundary
// test in test_amd_smu_address_probe exists to prevent.
//
// TableVersionId (RSMU_TABLE_VERSION_COMMAND) is read-only, and the only
// command the version probe issues.
pub use crate::hardware::lowlevel_io::{RSMU_ARG0, RSMU_MSG, RSMU_RSP, RSMU_TABLE_VERSION_COMMAND};

pub const WAIT_OBJECT_0: u32 = 0x0000_0000;
pub const WAIT_ABANDONED: u32 = 0x0000_0080;
pub const WAIT_TIMEOUT: u32 = 0x0000_0102;

/// CF8 value selecting bus 0, device 0, function 0, register `reg`.
pub const fn config_address(reg: u32) -> u32 {
    crate::hardware::lowlevel_io::config_address(reg)
}

/// A held cross-process lock; released when dropped.
pub type MutexGuard = Box<dyn Any>;

/// What serialises a transaction with other processes: the Python readers'
/// `mutex` context manager. By default a real named Windows mutex
/// ([`NamedPciMutex`]); tests supply their own to count entries and exits.
pub trait PciMutex: Send + Sync {
    /// Take the lock, or say why it could not be taken.
    fn acquire(&self) -> HwResult<MutexGuard>;
}

/// The named Windows mutex the mainstream monitoring tools share.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedPciMutex {
    pub name: String,
    pub timeout_ms: u32,
}

impl NamedPciMutex {
    pub fn new(name: &str) -> Self {
        NamedPciMutex { name: name.to_string(), timeout_ms: DEFAULT_MUTEX_TIMEOUT_MS }
    }
}

impl Default for NamedPciMutex {
    fn default() -> Self {
        Self::new(DEFAULT_MUTEX_NAME)
    }
}

impl PciMutex for NamedPciMutex {
    fn acquire(&self) -> HwResult<MutexGuard> {
        NamedMutex::acquire(&self.name, self.timeout_ms).map(|guard| Box::new(guard) as MutexGuard)
    }
}

/// An SMN reader as the AM5 runtime uses one: a batch read, a single read,
/// and the reason the last one failed. Implemented by the CF8/CFC transport
/// here and the ECAM transport in [`super::smn_mcfg`].
pub trait SmnTransport {
    /// Read several SMN dwords in one serialized transaction. Every address
    /// maps to its value, or to `None` if the transaction failed.
    fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>>;

    /// One 32-bit SMN value, or `None` on transaction failure.
    fn read(&self, smn_address: u32) -> Option<u32> {
        self.read_many(&[smn_address]).get(&smn_address).copied().flatten()
    }

    /// Why the last read failed; empty when it did not.
    fn last_error(&self) -> String;
}

/// Every address mapped to `None`.
pub(crate) fn all_none(addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
    addresses.iter().map(|address| (*address, None)).collect()
}

/// Serialized, vendor-gated reader for AM5 SMN registers.
///
/// Port I/O is [`hw`]; `mutex` is whatever serializes with other processes,
/// by default a real named Windows mutex.
pub struct SmnReader {
    mutex: Box<dyn PciMutex>,
    lock: Mutex<()>,
    last_error: Mutex<String>,
}

impl SmnReader {
    /// A reader on the shared `Global\Access_PCI` mutex.
    ///
    /// Fails, as the Python constructor did, when the driver DLL could not be
    /// loaded at all.
    pub fn new() -> Result<Self, String> {
        Self::with_mutex_name(DEFAULT_MUTEX_NAME)
    }

    pub fn with_mutex_name(mutex_name: &str) -> Result<Self, String> {
        if let Some(error) = crate::hardware::driver_error() {
            return Err(error);
        }
        Ok(Self::with_mutex(Box::new(NamedPciMutex::new(mutex_name))))
    }

    /// A reader serialised by `mutex` instead of the named mutex.
    pub fn with_mutex(mutex: Box<dyn PciMutex>) -> Self {
        SmnReader { mutex, lock: Mutex::new(()), last_error: Mutex::new(String::new()) }
    }

    fn set_last_error(&self, message: String) {
        *self.last_error.lock().unwrap_or_else(|e| e.into_inner()) = message;
    }

    // -- internal config-space helpers -------------------------------------
    fn read_config_dword(reg: u32) -> HwResult<u32> {
        let io = hw();
        io.outl(CF8_PORT, config_address(reg))?;
        io.inl(CFC_PORT)
    }

    fn write_config_dword(reg: u32, value: u32) -> HwResult<()> {
        let io = hw();
        io.outl(CF8_PORT, config_address(reg))?;
        io.outl(CFC_PORT, value)
    }

    /// The serialized part of [`SmnReader::read_many`]: `Ok(true)` when every
    /// value was read, `Ok(false)` when the vendor gate refused.
    fn transaction(&self, addresses: &[u32], results: &mut BTreeMap<u32, Option<u32>>) -> HwResult<bool> {
        let io = hw();
        let _lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        let _mutex = self.mutex.acquire()?;
        let prior_cf8 = io.inl(CF8_PORT)?;
        let outcome = (|| -> HwResult<bool> {
            let vendor = Self::read_config_dword(VENDOR_REG)? & 0xFFFF;
            if vendor != AMD_VENDOR_ID {
                self.set_last_error(format!("PCI 00:00.0 vendor is 0x{vendor:04X}, expected AMD 0x1022"));
                return Ok(false);
            }
            let prior_selector = Self::read_config_dword(SMN_INDEX_REG)?;
            let mut selector_attempted = false;
            let mut body = || -> HwResult<()> {
                for address in addresses {
                    // Mark before the call because a driver can report
                    // failure after partially completing the write.
                    selector_attempted = true;
                    Self::write_config_dword(SMN_INDEX_REG, *address)?;
                    let value = Self::read_config_dword(SMN_DATA_REG)?;
                    if value == 0xFFFF_FFFF {
                        return Err(HwError(format!("SMN 0x{address:08X} returned invalid 0xFFFFFFFF")));
                    }
                    results.insert(*address, Some(value));
                }
                Ok(())
            };
            let mut result = body();
            if selector_attempted {
                // A failing restore replaces whatever the body raised, as a
                // raise inside Python's `finally` does.
                if let Err(error) = Self::write_config_dword(SMN_INDEX_REG, prior_selector) {
                    result = Err(error);
                }
            }
            result.map(|()| true)
        })();
        match io.outl(CF8_PORT, prior_cf8) {
            Ok(()) => outcome,
            Err(error) => Err(error),
        }
    }
}

impl SmnTransport for SmnReader {
    /// Read several SMN dwords in one serialized PCI transaction.
    ///
    /// The named mutex, AMD vendor check, and CF8 save/restore happen once for
    /// the complete snapshot. Values are `None` if the transaction fails;
    /// genuine zero remains zero.
    fn read_many(&self, smn_addresses: &[u32]) -> BTreeMap<u32, Option<u32>> {
        let addresses = smn_addresses.to_vec();
        let mut results = all_none(&addresses);
        self.set_last_error(String::new());
        if !hw().is_driver_open() {
            self.set_last_error("InpOut driver is not open".into());
            return results;
        }
        match self.transaction(&addresses, &mut results) {
            Ok(true) => results,
            Ok(false) => all_none(&addresses),
            Err(error) => {
                self.set_last_error(format!("SMN read failed: {error}"));
                all_none(&addresses)
            }
        }
    }

    fn last_error(&self) -> String {
        self.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Transport-level tests for the AM5 SMN reader.
    //!
    //! These never touch hardware: a FakePortIo emulates the AMD PCI CF8/CFC
    //! config mechanism (bus/dev/func 00:00.0) and records every port access
    //! so the exact transport sequence, CF8 save/restore, vendor gating and
    //! failure handling can be asserted.

    use super::*;
    use crate::hardware::{Hardware, set_thread_hardware};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const PCI_ENABLE: u32 = 0x8000_0000;
    const CF8: u16 = 0xCF8;
    const CFC: u16 = 0xCFC;
    const SELECTOR_REG: u32 = 0x60;
    const DATA_REG: u32 = 0x64;
    const AMD_VENDOR: u32 = 0x1022;

    fn cfg(reg: u32) -> u32 {
        PCI_ENABLE | (reg & 0xFC)
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Call {
        IsOpen,
        Outl(u16, u32),
        Inl(u16),
    }

    struct PortState {
        cf8: u32,
        selector: u32,
        calls: Vec<Call>,
        failed: bool,
    }

    /// Emulates 00:00.0 config space + the SMN index/data window.
    struct FakePortIo {
        smn: BTreeMap<u32, u32>,
        vendor: u32,
        driver_open: bool,
        state: Mutex<PortState>,
        /// Fail the first selector write of this value, after making it.
        fail_selector_write: Option<u32>,
    }

    impl FakePortIo {
        fn new(smn: &[(u32, u32)]) -> Self {
            FakePortIo {
                smn: smn.iter().copied().collect(),
                vendor: AMD_VENDOR,
                driver_open: true,
                state: Mutex::new(PortState {
                    cf8: 0xDEAD_BEEF,
                    selector: 0xABCD_EF00,
                    calls: Vec::new(),
                    failed: false,
                }),
                fail_selector_write: None,
            }
        }

        fn initial_cf8(self, value: u32) -> Self {
            self.state.lock().unwrap().cf8 = value;
            self
        }

        fn initial_selector(self, value: u32) -> Self {
            self.state.lock().unwrap().selector = value;
            self
        }

        fn calls(&self) -> Vec<Call> {
            self.state.lock().unwrap().calls.clone()
        }

        fn cf8(&self) -> u32 {
            self.state.lock().unwrap().cf8
        }

        fn selector(&self) -> u32 {
            self.state.lock().unwrap().selector
        }
    }

    impl Hardware for FakePortIo {
        fn read_phys(&self, _address: u64, _size: usize) -> HwResult<Vec<u8>> {
            panic!("unexpected physical read")
        }
        fn get_phys_long(&self, _address: u64) -> HwResult<u32> {
            panic!("unexpected physical read")
        }
        fn set_phys_long(&self, _address: u64, _value: u32) -> HwResult<()> {
            panic!("unexpected physical write")
        }
        fn inb(&self, port: u16) -> HwResult<u8> {
            panic!("unexpected inb port 0x{port:X}")
        }
        fn outb(&self, port: u16, _value: u8) -> HwResult<()> {
            panic!("unexpected outb port 0x{port:X}")
        }
        fn inl(&self, port: u16) -> HwResult<u32> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call::Inl(port));
            if port == CF8 {
                return Ok(state.cf8);
            }
            if port == CFC {
                if state.cf8 == cfg(VENDOR_REG) {
                    return Ok(self.vendor & 0xFFFF);
                }
                if state.cf8 == cfg(SELECTOR_REG) {
                    return Ok(state.selector);
                }
                if state.cf8 == cfg(DATA_REG) {
                    return Ok(self.smn.get(&state.selector).copied().unwrap_or(0));
                }
                return Ok(0);
            }
            panic!("unexpected inl port 0x{port:X}")
        }
        fn outl(&self, port: u16, value: u32) -> HwResult<()> {
            let mut state = self.state.lock().unwrap();
            state.calls.push(Call::Outl(port, value));
            if port == CF8 {
                state.cf8 = value;
            } else if port == CFC {
                if state.cf8 == cfg(SELECTOR_REG) {
                    state.selector = value;
                    if !state.failed && self.fail_selector_write == Some(value) {
                        state.failed = true;
                        return Err(HwError::new("reported write failure"));
                    }
                }
                // a write while pointed at DATA_REG would be an illegal SMN write
            } else {
                panic!("unexpected outl port 0x{port:X}");
            }
            Ok(())
        }
        fn is_driver_open(&self) -> bool {
            self.state.lock().unwrap().calls.push(Call::IsOpen);
            self.driver_open
        }
    }

    fn install(io: FakePortIo) -> Arc<FakePortIo> {
        let shared = Arc::new(io);
        set_thread_hardware(Some(shared.clone()));
        shared
    }

    /// Counts entries and exits, like the Python tests' FakeMutex.
    #[derive(Clone, Default)]
    pub(crate) struct FakeMutex {
        pub entered: Arc<AtomicUsize>,
        pub exited: Arc<AtomicUsize>,
    }

    struct FakeGuard(Arc<AtomicUsize>);

    impl Drop for FakeGuard {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl FakeMutex {
        pub(crate) fn counts(&self) -> (usize, usize) {
            (self.entered.load(Ordering::SeqCst), self.exited.load(Ordering::SeqCst))
        }
    }

    impl PciMutex for FakeMutex {
        fn acquire(&self) -> HwResult<MutexGuard> {
            self.entered.fetch_add(1, Ordering::SeqCst);
            Ok(Box::new(FakeGuard(self.exited.clone())))
        }
    }

    fn reader() -> SmnReader {
        SmnReader::with_mutex(Box::new(FakeMutex::default()))
    }

    // -- NamedMutexTest ------------------------------------------------------
    //
    // The Python tests drove NamedMutex through a fake kernel32. Here the
    // real mutex is driven into the same two states.

    #[test]
    fn timeout_raises_without_releasing_unowned_mutex() {
        let name = format!("Local\\RochViewerSmnTimeout{}", std::process::id());
        let holder_name = name.clone();
        let (held_tx, held_rx) = std::sync::mpsc::channel();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
        let holder = std::thread::spawn(move || {
            let guard = NamedMutex::acquire(&holder_name, 1000).expect("holder acquires");
            held_tx.send(()).unwrap();
            done_rx.recv().unwrap();
            drop(guard);
        });
        held_rx.recv().unwrap();
        let error = NamedPciMutex { name: name.clone(), timeout_ms: 1 }
            .acquire()
            .expect_err("times out");
        assert_eq!(error.0, "Timed out waiting for PCI configuration mutex");
        done_tx.send(()).unwrap();
        holder.join().unwrap();
        // The holder still released its own mutex: it is free again.
        drop(NamedMutex::acquire(&name, 100).expect("free after the holder released"));
    }

    #[test]
    fn abandoned_mutex_is_owned_and_released() {
        let name = format!("Local\\RochViewerSmnAbandoned{}", std::process::id());
        let holder_name = name.clone();
        std::thread::spawn(move || {
            // Exit the thread still holding it: the mutex is abandoned.
            std::mem::forget(NamedMutex::acquire(&holder_name, 1000).expect("holder acquires"));
        })
        .join()
        .unwrap();
        let guard = NamedPciMutex { name: name.clone(), timeout_ms: 100 }
            .acquire()
            .expect("abandoned counts as acquired");
        drop(guard);
        // Released on drop: another thread can take it straight away.
        std::thread::spawn(move || drop(NamedMutex::acquire(&name, 100).expect("released")))
            .join()
            .unwrap();
    }

    // -- SmnReadTest ---------------------------------------------------------

    #[test]
    fn all_ones_invalidates_complete_batch() {
        install(FakePortIo::new(&[(0x50204, 0x1111), (0x50208, 0xFFFF_FFFF)]));
        let reader = reader();
        assert_eq!(
            reader.read_many(&[0x50204, 0x50208]),
            BTreeMap::from([(0x50204, None), (0x50208, None)])
        );
        assert!(reader.last_error().to_lowercase().contains("0xffffffff"));
    }

    #[test]
    fn returns_mapped_value() {
        install(FakePortIo::new(&[(0x50204, 0x1234_ABCD)]));
        assert_eq!(reader().read(0x50204), Some(0x1234_ABCD));
    }

    #[test]
    fn zero_is_not_failure() {
        install(FakePortIo::new(&[(0x50204, 0)]));
        assert_eq!(reader().read(0x50204), Some(0));
    }

    #[test]
    fn transport_sequence() {
        let io = install(FakePortIo::new(&[(0x50208, 0xCAFE)]));
        assert_eq!(reader().read(0x50208), Some(0xCAFE));
        assert_eq!(
            io.calls(),
            vec![
                Call::IsOpen,
                Call::Inl(CF8),                       // save prior CF8
                Call::Outl(CF8, cfg(VENDOR_REG)),     // vendor check
                Call::Inl(CFC),
                Call::Outl(CF8, cfg(SELECTOR_REG)),   // save SMN selector
                Call::Inl(CFC),
                Call::Outl(CF8, cfg(SELECTOR_REG)),   // selector write
                Call::Outl(CFC, 0x50208),
                Call::Outl(CF8, cfg(DATA_REG)),       // data read
                Call::Inl(CFC),
                Call::Outl(CF8, cfg(SELECTOR_REG)),   // restore selector
                Call::Outl(CFC, 0xABCD_EF00),
                Call::Outl(CF8, 0xDEAD_BEEF),         // restore prior CF8
            ]
        );
    }

    #[test]
    fn restores_prior_smn_selector() {
        let io = install(FakePortIo::new(&[(0x50208, 1)]).initial_selector(0x1122_3344));
        reader().read(0x50208);
        assert_eq!(io.selector(), 0x1122_3344);
    }

    #[test]
    fn failed_selector_write_still_attempts_restore() {
        let mut fake = FakePortIo::new(&[]).initial_selector(0x1122_3344);
        fake.fail_selector_write = Some(0x50208);
        let io = install(fake);
        assert_eq!(reader().read(0x50208), None);
        assert_eq!(io.selector(), 0x1122_3344);
    }

    #[test]
    fn restores_prior_cf8() {
        let io = install(FakePortIo::new(&[(0x50208, 1)]).initial_cf8(0x1122_3344));
        reader().read(0x50208);
        assert_eq!(io.cf8(), 0x1122_3344);
        assert_eq!(io.calls().last(), Some(&Call::Outl(CF8, 0x1122_3344)));
    }

    #[test]
    fn vendor_rejection() {
        let mut fake = FakePortIo::new(&[(0x50208, 0xCAFE)]);
        fake.vendor = 0x8086;
        let io = install(fake);
        let reader = reader();
        assert_eq!(reader.read(0x50208), None);
        assert!(reader.last_error().to_lowercase().contains("vendor"));
        // never selects the SMN index register on a foreign vendor
        assert!(!io.calls().contains(&Call::Outl(CF8, cfg(SELECTOR_REG))));
        // CF8 still restored
        assert_eq!(io.calls().last(), Some(&Call::Outl(CF8, 0xDEAD_BEEF)));
    }

    #[test]
    fn only_selector_writes() {
        let io = install(FakePortIo::new(&[(0x50208, 0xCAFE)]));
        reader().read(0x50208);
        let cfc_writes: Vec<Call> = io
            .calls()
            .into_iter()
            .filter(|call| matches!(call, Call::Outl(port, _) if *port == CFC))
            .collect();
        // the only data written to CFC is the selector value; the code must
        // never write to the SMN data register.
        assert_eq!(cfc_writes, vec![Call::Outl(CFC, 0x50208), Call::Outl(CFC, 0xABCD_EF00)]);
    }

    #[test]
    fn driver_closed_is_failure() {
        let mut fake = FakePortIo::new(&[(0x50208, 0xCAFE)]);
        fake.driver_open = false;
        let io = install(fake);
        let reader = reader();
        assert_eq!(reader.read(0x50208), None);
        assert!(reader.last_error().to_lowercase().contains("driver"));
        // no port I/O performed when the driver is unavailable
        assert_eq!(io.calls(), vec![Call::IsOpen]);
    }

    #[test]
    fn no_public_write_api() {
        // Python checked that SmnReader has no `write` / `smn_write`
        // attribute. The Rust equivalent: nothing outside the test module of
        // this file defines a write method on the reader.
        let source = include_str!("smn.rs");
        let shipped = source.split("#[cfg(test)]").next().unwrap();
        assert!(!shipped.contains("fn write("));
        assert!(!shipped.contains("fn smn_write("));
    }

    #[test]
    fn mutex_wraps_access() {
        install(FakePortIo::new(&[(0x50208, 1)]));
        let mutex = FakeMutex::default();
        SmnReader::with_mutex(Box::new(mutex.clone())).read(0x50208);
        assert_eq!(mutex.counts(), (1, 1));
    }

    #[test]
    fn batch_holds_mutex_and_checks_vendor_once() {
        let io = install(FakePortIo::new(&[(0x50204, 0x1111), (0x50208, 0x2222)]));
        let mutex = FakeMutex::default();
        let reader = SmnReader::with_mutex(Box::new(mutex.clone()));
        let values = reader.read_many(&[0x50204, 0x50208]);
        assert_eq!(values, BTreeMap::from([(0x50204, Some(0x1111)), (0x50208, Some(0x2222))]));
        assert_eq!(mutex.counts(), (1, 1));
        let vendor_selects = io
            .calls()
            .into_iter()
            .filter(|call| *call == Call::Outl(CF8, cfg(VENDOR_REG)))
            .count();
        assert_eq!(vendor_selects, 1);
        assert_eq!(io.calls().last(), Some(&Call::Outl(CF8, 0xDEAD_BEEF)));
    }
}
