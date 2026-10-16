//! Low-level access: the driver, port and physical-memory access, PCIe
//! configuration space.
//!
//! Every privileged reading in the program goes through [`Hardware`]. The real
//! implementation is the third-party inpoutx64 driver, loaded on first use;
//! when it is absent the backend is one that fails every access with the
//! reason, which is a condition, not a crash -- the viewer still opens and
//! says so. Tests install their own backend for their thread with
//! [`set_thread_hardware`], which is how the Python tests' patched readers are
//! ported.
//!
//! There is deliberately no general write API. The writes that exist are the
//! port selectors the transports need (CF8, SMBus host registers, Super I/O
//! index ports) and `set_phys_long` for the SMU mailbox, exactly as the Python
//! had them.

pub mod driver_path;
pub mod inpout;
pub mod lowlevel_io;
pub mod pci_mcfg;
pub mod read;

use std::cell::RefCell;
use std::fmt;
use std::sync::{Arc, OnceLock};

/// Why an access did not happen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HwError(pub String);

impl HwError {
    pub fn new(message: impl Into<String>) -> Self {
        HwError(message.into())
    }
}

impl fmt::Display for HwError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for HwError {}

pub type HwResult<T> = Result<T, HwError>;

/// The privileged primitives the readers are built from.
pub trait Hardware: Send + Sync {
    /// `size` bytes of physical memory (MMIO included), little-endian as laid
    /// out. Reads are done as aligned 32-bit loads where the size allows,
    /// which is what memory-controller registers expect.
    fn read_phys(&self, address: u64, size: usize) -> HwResult<Vec<u8>>;
    /// inpoutx64 `GetPhysLong`: one 32-bit physical read.
    fn get_phys_long(&self, address: u64) -> HwResult<u32>;
    /// inpoutx64 `SetPhysLong`: one 32-bit physical write. Used only for the
    /// SMU mailbox registers, as the Python did.
    fn set_phys_long(&self, address: u64, value: u32) -> HwResult<()>;
    fn inb(&self, port: u16) -> HwResult<u8>;
    fn outb(&self, port: u16, value: u8) -> HwResult<()>;
    fn inl(&self, port: u16) -> HwResult<u32>;
    fn outl(&self, port: u16, value: u32) -> HwResult<()>;
    /// inpoutx64 `IsInpOutDriverOpen`.
    fn is_driver_open(&self) -> bool;
}

/// The backend used when no driver could be loaded: every access fails with
/// the reason.
pub struct Unavailable(pub String);

impl Hardware for Unavailable {
    fn read_phys(&self, _address: u64, _size: usize) -> HwResult<Vec<u8>> {
        Err(HwError(self.0.clone()))
    }
    fn get_phys_long(&self, _address: u64) -> HwResult<u32> {
        Err(HwError(self.0.clone()))
    }
    fn set_phys_long(&self, _address: u64, _value: u32) -> HwResult<()> {
        Err(HwError(self.0.clone()))
    }
    fn inb(&self, _port: u16) -> HwResult<u8> {
        Err(HwError(self.0.clone()))
    }
    fn outb(&self, _port: u16, _value: u8) -> HwResult<()> {
        Err(HwError(self.0.clone()))
    }
    fn inl(&self, _port: u16) -> HwResult<u32> {
        Err(HwError(self.0.clone()))
    }
    fn outl(&self, _port: u16, _value: u32) -> HwResult<()> {
        Err(HwError(self.0.clone()))
    }
    fn is_driver_open(&self) -> bool {
        false
    }
}

thread_local! {
    static THREAD_HARDWARE: RefCell<Option<Arc<dyn Hardware>>> = const { RefCell::new(None) };
}

/// Replace the hardware backend for the calling thread only; `None` restores
/// the process backend. Intended for tests.
pub fn set_thread_hardware(hardware: Option<Arc<dyn Hardware>>) {
    THREAD_HARDWARE.with(|slot| *slot.borrow_mut() = hardware);
}

static PROCESS_HARDWARE: OnceLock<Arc<dyn Hardware>> = OnceLock::new();

fn process_hardware() -> Arc<dyn Hardware> {
    PROCESS_HARDWARE
        .get_or_init(|| {
            if cfg!(test) {
                // Unit tests never touch the machine they run on.
                return Arc::new(Unavailable("no hardware in unit tests".into()));
            }
            match inpout::InpOut::load() {
                Ok(driver) => Arc::new(driver),
                Err(error) => Arc::new(Unavailable(error.0)),
            }
        })
        .clone()
}

/// The backend every reader uses.
pub fn hw() -> Arc<dyn Hardware> {
    THREAD_HARDWARE
        .with(|slot| slot.borrow().clone())
        .unwrap_or_else(process_hardware)
}

/// Why the driver is unusable, or `None` when it loaded. The window shows
/// this rather than a screen full of blanks with no explanation.
pub fn driver_error() -> Option<String> {
    if cfg!(test) {
        return None;
    }
    inpout::InpOut::load_error()
}
