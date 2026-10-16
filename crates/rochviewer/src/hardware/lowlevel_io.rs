//! Serialised low-level access: a named mutex and the PCI configuration
//! mechanism over ports CF8/CFC.
//!
//! Two pieces the readers share:
//!
//! * [`NamedMutex`], the Windows named mutex the mainstream monitoring tools
//!   take before touching the shared CF8/CFC configuration window, so two
//!   tools do not clobber each other's selector mid-read.
//! * The CF8/CFC constants and the driver-open pre-check.
//!
//! Nothing here is vendor-specific, and there is deliberately no write API
//! beyond the port primitives the transports need for their own selectors.

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
use windows::core::HSTRING;

use super::{HwError, HwResult};

pub const CF8_PORT: u16 = 0xCF8; // config address port
pub const CFC_PORT: u16 = 0xCFC; // config data port
pub const PCI_CONFIG_ENABLE: u32 = 0x8000_0000;

pub const VENDOR_REG: u32 = 0x00; // DWORD: [15:0] vendor id, [31:16] device id
pub const SMN_INDEX_REG: u32 = 0x60; // selector: SMN address goes here
pub const SMN_DATA_REG: u32 = 0x64; // data window: SMN value read here

pub const AMD_VENDOR_ID: u32 = 0x1022;

pub const RSMU_MSG: u32 = 0x3B1_0524;
pub const RSMU_RSP: u32 = 0x3B1_0570;
pub const RSMU_ARG0: u32 = 0x3B1_0A40;

pub const RSMU_TABLE_VERSION_COMMAND: u32 = 0x05;

pub const DEFAULT_MUTEX_NAME: &str = "Global\\Access_PCI";
pub const DEFAULT_MUTEX_TIMEOUT_MS: u32 = 5000;

/// CF8 value selecting bus 0, device 0, function 0, register `reg`.
pub const fn config_address(reg: u32) -> u32 {
    PCI_CONFIG_ENABLE | (reg & 0xFC)
}

/// CF8 value for any bus/device/function/register.
pub const fn config_address_bdf(bus: u32, device: u32, function: u32, reg: u32) -> u32 {
    PCI_CONFIG_ENABLE
        | ((bus & 0xFF) << 16)
        | ((device & 0x1F) << 11)
        | ((function & 0x7) << 8)
        | (reg & 0xFC)
}

/// A held named Windows mutex; released and closed on drop.
pub struct NamedMutex {
    handle: HANDLE,
}

// SAFETY: a mutex handle may be released from the thread that acquired it;
// the guard is not moved across threads by any caller, but HANDLE itself is
// just a kernel object reference.
unsafe impl Send for NamedMutex {}

impl NamedMutex {
    /// Take the mutex `name`, waiting at most `timeout_ms`.
    ///
    /// An abandoned mutex (another tool died holding it) counts as acquired,
    /// as it does for the tools that share it.
    pub fn acquire(name: &str, timeout_ms: u32) -> HwResult<Self> {
        // SAFETY: plain Win32 calls on a handle this function owns.
        unsafe {
            let handle = CreateMutexW(None, false, &HSTRING::from(name))
                .map_err(|e| HwError(format!("CreateMutexW failed: {e}")))?;
            let wait = WaitForSingleObject(handle, timeout_ms);
            if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
                return Ok(NamedMutex { handle });
            }
            let _ = CloseHandle(handle);
            if wait == WAIT_TIMEOUT {
                Err(HwError("Timed out waiting for PCI configuration mutex".into()))
            } else {
                Err(HwError(format!(
                    "WaitForSingleObject failed: {}",
                    windows::core::Error::from_thread()
                )))
            }
        }
    }

    /// The shared PCI-configuration mutex with the default timeout.
    pub fn pci() -> HwResult<Self> {
        Self::acquire(DEFAULT_MUTEX_NAME, DEFAULT_MUTEX_TIMEOUT_MS)
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and currently held by this thread.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_address_selects_bus0_dev0_fn0_and_aligns() {
        assert_eq!(config_address(0x60), 0x8000_0060);
        assert_eq!(config_address(0x67), 0x8000_0064);
    }

    #[test]
    fn bdf_encoding() {
        assert_eq!(config_address_bdf(0, 0x1F, 4, 0x20), 0x8000_FC20);
    }

    #[test]
    fn mutex_round_trip() {
        let name = format!("Local\\RochViewerTest{}", std::process::id());
        let held = NamedMutex::acquire(&name, 100).expect("acquire");
        drop(held);
        let again = NamedMutex::acquire(&name, 100).expect("reacquire");
        drop(again);
    }
}
