//! Low-level access: the driver, port and physical-memory access, PCIe
//! configuration space.
//!
//! Every privileged reading in the program goes through [`Hardware`]. The real
//! implementation is the third-party inpoutx64 driver on Windows, loaded on
//! first use, and on Linux [`linux`]: the kernel's own interfaces, reached from
//! a small privileged helper. When neither can be had the backend is one that
//! fails every access with the reason, which is a condition, not a crash -- the
//! viewer still opens and says so. Tests install their own backend for their
//! thread with [`set_thread_hardware`], which is how the Python tests' patched
//! readers are ported.
//!
//! There is deliberately no general write API. The writes that exist are the
//! port selectors the transports need (CF8, SMBus host registers, Super I/O
//! index ports) and `set_phys_long` for the SMU mailbox, exactly as the Python
//! had them.

pub mod driver_path;
#[cfg(windows)]
pub mod inpout;
#[cfg(target_os = "linux")]
pub mod linux;
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

/// Python's `str(OSError(errno, message))`: "[Errno 5] CreateMutexW failed".
/// The readers' status lines carried this text, so it is kept word for word.
pub fn os_error(code: u32, message: &str) -> String {
    format!("[Errno {code}] {message}")
}

/// The calling thread's last Win32 error, as ctypes.get_last_error() gave it.
#[cfg(windows)]
pub fn last_win32_error() -> u32 {
    // SAFETY: reads a thread-local value.
    unsafe { windows::Win32::Foundation::GetLastError().0 }
}

/// The Win32 code inside a windows-crate error (HRESULT_FROM_WIN32 undone).
#[cfg(windows)]
pub fn win32_code(error: &windows::core::Error) -> u32 {
    let hresult = error.code().0 as u32;
    if hresult & 0xFFFF_0000 == 0x8007_0000 { hresult & 0xFFFF } else { hresult }
}

/// One SMBus transfer for the kernel's own adapter driver to carry out.
///
/// Linux only in practice: where the kernel owns the controller, this is how
/// its lock is shared rather than raced. See `intel_pch_smbus`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KernelSmbus {
    /// Read Byte Data.
    ReadByte { register: u8 },
    /// Read Word Data; the low byte is the first on the wire.
    ReadWord { register: u8 },
    /// Write Byte Data, to an allowlisted selector.
    WriteByte { register: u8, value: u8 },
    /// Send Byte: DDR4's EE1004 page select.
    SendByte { value: u8 },
}

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
    /// A firmware table the OS only shows an administrator: ACPI's MCFG and the
    /// SMBIOS table under `/sys/firmware`. Windows hands those out through
    /// ordinary API calls, so only the Linux backends answer this.
    fn read_firmware_file(&self, path: &str) -> HwResult<Vec<u8>> {
        Err(HwError(format!("{path} cannot be read through this backend")))
    }
    /// One SMBus transfer through kernel adapter number `adapter`.
    fn kernel_smbus(&self, _adapter: u32, _address: u8, _transfer: KernelSmbus) -> HwResult<u16> {
        Err(HwError::new("this backend has no kernel SMBus adapter"))
    }
    /// One byte of EC RAM, through the kernel's own EC driver.
    fn kernel_ec_byte(&self, _register: u8) -> HwResult<u8> {
        Err(HwError::new("this backend has no kernel EC access"))
    }
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
    fn read_firmware_file(&self, _path: &str) -> HwResult<Vec<u8>> {
        Err(HwError(self.0.clone()))
    }
    fn kernel_smbus(&self, _adapter: u32, _address: u8, _transfer: KernelSmbus) -> HwResult<u16> {
        Err(HwError(self.0.clone()))
    }
    fn kernel_ec_byte(&self, _register: u8) -> HwResult<u8> {
        Err(HwError(self.0.clone()))
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

#[cfg(windows)]
fn open_backend() -> Arc<dyn Hardware> {
    match inpout::InpOut::load() {
        Ok(driver) => Arc::new(driver),
        Err(error) => Arc::new(Unavailable(error.0)),
    }
}

#[cfg(target_os = "linux")]
fn open_backend() -> Arc<dyn Hardware> {
    linux::open()
}

#[cfg(not(any(windows, target_os = "linux")))]
fn open_backend() -> Arc<dyn Hardware> {
    Arc::new(Unavailable("Register access is not available on this operating system.".into()))
}

fn process_hardware() -> Arc<dyn Hardware> {
    PROCESS_HARDWARE
        .get_or_init(|| {
            if cfg!(test) {
                // Unit tests never touch the machine they run on.
                return Arc::new(Unavailable("no hardware in unit tests".into()));
            }
            open_backend()
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
    #[cfg(windows)]
    return inpout::InpOut::load_error();
    #[cfg(target_os = "linux")]
    return linux::load_error();
    #[cfg(not(any(windows, target_os = "linux")))]
    return Some("Register access is not available on this operating system.".into());
}

#[cfg(test)]
mod error_text_tests {
    use super::*;

    #[test]
    fn os_errors_read_as_python_printed_them() {
        assert_eq!(os_error(5, "CreateMutexW failed"), "[Errno 5] CreateMutexW failed");
        assert_eq!(os_error(0, "Reading the MCFG table failed"), "[Errno 0] Reading the MCFG table failed");
    }

    #[cfg(windows)]
    #[test]
    fn win32_codes_come_out_of_hresults() {
        let denied = windows::core::Error::from_hresult(windows::core::HRESULT(0x8007_0005u32 as i32));
        assert_eq!(win32_code(&denied), 5);
    }
}
