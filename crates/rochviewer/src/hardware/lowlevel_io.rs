//! Serialised low-level access: a named mutex and the PCI configuration
//! mechanism over ports CF8/CFC.
//!
//! Two pieces the readers share:
//!
//! * [`NamedMutex`], the Windows named mutex the mainstream monitoring tools
//!   take before touching the shared CF8/CFC configuration window, so two
//!   tools do not clobber each other's selector mid-read. On Linux no other tool
//!   takes it, so it serialises this program's own threads, and other copies of
//!   it, with the same behaviour: re-entrant, and an abandoned one is acquired.
//! * The CF8/CFC constants and the driver-open pre-check.
//!
//! Nothing here is vendor-specific, and there is deliberately no write API
//! beyond the port primitives the transports need for their own selectors.

#[cfg(windows)]
use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_ABANDONED, WAIT_OBJECT_0, WAIT_TIMEOUT};
#[cfg(windows)]
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex, WaitForSingleObject};
#[cfg(windows)]
use windows::core::HSTRING;

#[cfg(windows)]
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
#[cfg(windows)]
pub struct NamedMutex {
    handle: HANDLE,
}

// SAFETY: a mutex handle may be released from the thread that acquired it;
// the guard is not moved across threads by any caller, but HANDLE itself is
// just a kernel object reference.
#[cfg(windows)]
unsafe impl Send for NamedMutex {}

#[cfg(windows)]
impl NamedMutex {
    /// Take the mutex `name`, waiting at most `timeout_ms`.
    ///
    /// An abandoned mutex (another tool died holding it) counts as acquired,
    /// as it does for the tools that share it.
    pub fn acquire(name: &str, timeout_ms: u32) -> HwResult<Self> {
        // SAFETY: plain Win32 calls on a handle this function owns.
        unsafe {
            let handle = CreateMutexW(None, false, &HSTRING::from(name))
                .map_err(|e| HwError(super::os_error(super::win32_code(&e), "CreateMutexW failed")))?;
            let wait = WaitForSingleObject(handle, timeout_ms);
            if wait == WAIT_OBJECT_0 || wait == WAIT_ABANDONED {
                return Ok(NamedMutex { handle });
            }
            let _ = CloseHandle(handle);
            if wait == WAIT_TIMEOUT {
                Err(HwError("Timed out waiting for PCI configuration mutex".into()))
            } else {
                Err(HwError(super::os_error(super::last_win32_error(), "WaitForSingleObject failed")))
            }
        }
    }

    /// The shared PCI-configuration mutex with the default timeout.
    pub fn pci() -> HwResult<Self> {
        Self::acquire(DEFAULT_MUTEX_NAME, DEFAULT_MUTEX_TIMEOUT_MS)
    }
}

#[cfg(windows)]
impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: the handle is owned and currently held by this thread.
        unsafe {
            let _ = ReleaseMutex(self.handle);
            let _ = CloseHandle(self.handle);
        }
    }
}

#[cfg(not(windows))]
pub use posix_mutex::{NamedMutex, superio_owned_by_kernel};

/// The same contract with no Windows to supply it.
///
/// Within the process: a Windows mutex belongs to a thread, may be taken again by
/// that thread, and counts as acquired when its owner died holding it, and callers
/// here lean on all three. Between processes: an advisory `flock` on a file named
/// for the mutex, which the kernel releases however the holder ended.
#[cfg(not(windows))]
mod posix_mutex {
    use std::collections::HashMap;
    use std::fs::File;
    use std::os::fd::AsRawFd;
    use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
    use std::time::{Duration, Instant};

    use super::super::{HwError, HwResult};

    #[derive(Default)]
    struct Owner {
        /// Alive for as long as the owning thread is: see [`thread_token`].
        thread: Option<Weak<()>>,
        depth: u32,
        file: Option<File>,
    }

    #[derive(Default)]
    struct Entry {
        owner: Mutex<Owner>,
        released: Condvar,
    }

    thread_local! {
        static THREAD_TOKEN: Arc<()> = Arc::new(());
    }

    /// A value that dies with this thread. A plain thread id would not say that
    /// the owner has gone, which is what makes a mutex abandoned.
    fn thread_token() -> Arc<()> {
        THREAD_TOKEN.with(Arc::clone)
    }

    fn entry(name: &str) -> Arc<Entry> {
        static ENTRIES: OnceLock<Mutex<HashMap<String, Arc<Entry>>>> = OnceLock::new();
        let entries = ENTRIES.get_or_init(Default::default);
        let mut entries = entries.lock().unwrap_or_else(|e| e.into_inner());
        entries.entry(name.to_string()).or_default().clone()
    }

    /// `Global\Access_PCI` as a file name, in the user's runtime directory where
    /// there is one. Failing to make the file only loses the between-processes
    /// half; this program's own threads are still serialised.
    fn lock_file(name: &str) -> Option<File> {
        let safe: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
        let dir = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir);
        std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!(".rochviewer-{safe}.lock")))
            .ok()
    }

    /// Whether a kernel driver is using the Super I/O, whose name it publishes.
    ///
    /// The chip is one index/data pair, and the bank register behind it is state.
    /// A kernel driver cannot be locked against from here, and its register
    /// accesses interleaving with these can read the wrong bank, or, on the fan
    /// path, write to it, and can wedge the embedded controller on the chips that
    /// have one. So the Super I/O mutex simply cannot be had while one is bound,
    /// and every caller already reads that as "do not touch the hardware". The
    /// answer is kept: a driver loaded after this looked is its loader's choice.
    pub fn superio_owned_by_kernel() -> Option<&'static str> {
        static OWNER: OnceLock<Option<String>> = OnceLock::new();
        OWNER
            .get_or_init(|| {
                std::fs::read_dir("/sys/class/hwmon")
                    .ok()?
                    .flatten()
                    .filter_map(|entry| std::fs::read_to_string(entry.path().join("name")).ok())
                    .map(|name| name.trim().to_string())
                    .find(|name| name.starts_with("nct6") || name.starts_with("it8"))
            })
            .as_deref()
    }

    /// A held named mutex; released on drop.
    pub struct NamedMutex {
        entry: Arc<Entry>,
    }

    impl NamedMutex {
        /// Take the mutex `name`, waiting at most `timeout_ms`.
        pub fn acquire(name: &str, timeout_ms: u32) -> HwResult<Self> {
            if name == crate::sensors::superio_lpc::ISA_MUTEX_NAME
                && let Some(driver) = superio_owned_by_kernel()
            {
                return Err(HwError(format!("The kernel's {driver} driver is using the Super I/O")));
            }
            let entry = entry(name);
            let deadline = Instant::now() + Duration::from_millis(timeout_ms as u64);
            let token = thread_token();
            let timed_out = || HwError("Timed out waiting for PCI configuration mutex".into());
            {
                let mut owner = entry.owner.lock().unwrap_or_else(|e| e.into_inner());
                loop {
                    let holder = owner.thread.as_ref().map(Weak::upgrade);
                    match holder {
                        // Free, or its thread has gone: abandoned, so acquired.
                        None | Some(None) => {
                            owner.depth = 0;
                            owner.file = None;
                            break;
                        }
                        Some(Some(held)) if Arc::ptr_eq(&held, &token) => {
                            owner.depth += 1;
                            drop(owner);
                            return Ok(NamedMutex { entry });
                        }
                        Some(Some(_)) => {}
                    }
                    let now = Instant::now();
                    if now >= deadline {
                        return Err(timed_out());
                    }
                    // Short, because a thread dying while holding this signals
                    // nothing.
                    let wait = (deadline - now).min(Duration::from_millis(10));
                    owner = entry.released.wait_timeout(owner, wait).unwrap_or_else(|e| e.into_inner()).0;
                }
                owner.thread = Some(Arc::downgrade(&token));
                owner.depth = 1;
            }
            // The other processes, outside the lock so that this thread waiting
            // on one does not hold up the rest of this one.
            let file = lock_file(name);
            if let Some(file) = &file {
                loop {
                    // SAFETY: an fd this function owns.
                    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                        break;
                    }
                    if Instant::now() >= deadline {
                        let mut owner = entry.owner.lock().unwrap_or_else(|e| e.into_inner());
                        owner.thread = None;
                        owner.depth = 0;
                        drop(owner);
                        entry.released.notify_one();
                        return Err(timed_out());
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            entry.owner.lock().unwrap_or_else(|e| e.into_inner()).file = file;
            Ok(NamedMutex { entry })
        }

        /// The shared PCI-configuration mutex with the default timeout.
        pub fn pci() -> HwResult<Self> {
            Self::acquire(super::DEFAULT_MUTEX_NAME, super::DEFAULT_MUTEX_TIMEOUT_MS)
        }
    }

    impl Drop for NamedMutex {
        fn drop(&mut self) {
            let mut owner = self.entry.owner.lock().unwrap_or_else(|e| e.into_inner());
            owner.depth = owner.depth.saturating_sub(1);
            if owner.depth == 0 {
                owner.thread = None;
                // Closing the file is what releases the flock.
                owner.file = None;
                drop(owner);
                self.entry.released.notify_one();
            }
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

    #[test]
    fn a_thread_can_take_its_own_mutex_again() {
        let name = format!("Local\\RochViewerReentrant{}", std::process::id());
        let first = NamedMutex::acquire(&name, 100).expect("acquire");
        let second = NamedMutex::acquire(&name, 100).expect("the same thread takes it again");
        drop(first);
        // Still held by the inner guard.
        let other = name.clone();
        let refused = std::thread::spawn(move || NamedMutex::acquire(&other, 20).is_err()).join().unwrap();
        assert!(refused, "held until the last guard is dropped");
        drop(second);
        let other = name.clone();
        let free = std::thread::spawn(move || NamedMutex::acquire(&other, 100).is_ok()).join().unwrap();
        assert!(free, "free once it is");
    }
}
