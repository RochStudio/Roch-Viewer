//! Register access on Linux.
//!
//! Windows gets its access from a third-party kernel driver, and the whole
//! process is elevated to reach it. Linux already has the primitives --
//! `/dev/mem`, `iopl`, PCI configuration through sysfs, `/dev/i2c-*` -- but they
//! need `CAP_SYS_RAWIO`, and a Wayland client should not run as root: the
//! compositor's socket, the settings and the font cache all belong to the person
//! at the desk.
//!
//! So the window stays an ordinary user process and the privileged half is a
//! small helper, this same executable started with [`HELPER_ARG`]. The two talk
//! over pipes, one [`Hardware`] call to a frame ([`protocol`]), and nothing else
//! crosses: the helper has no general read or write, and it still refuses what
//! [`direct`] refuses however it is asked.
//!
//! In order of preference, the backend is:
//!
//! 1. [`direct::Direct`], in this process, when it already has the access: it
//!    was started as root, or the binary carries the capabilities.
//! 2. The helper `rochviewer setup` installed, which needs no prompt.
//! 3. This executable as the helper, through `pkexec`: the polkit prompt, which
//!    is the same question Windows asks with UAC on every launch.
//!
//! When none of them can start the viewer still opens and says why, as it does
//! on Windows with no driver.

pub mod direct;
pub mod helper;
pub mod protocol;
pub mod proxy;

use std::sync::{Arc, OnceLock};

use super::{Hardware, Unavailable};

/// The argument that makes this executable the privileged helper.
pub const HELPER_ARG: &str = "--hardware-helper";

/// Where `rochviewer setup` installs a root-owned copy of the helper, carrying
/// the capabilities it needs so that starting it asks nothing.
pub const INSTALLED_HELPER: &str = "/usr/local/libexec/rochviewer-helper";

/// The installed helper's file name, which makes the program nothing but that.
pub const HELPER_FILE_NAME: &str = "rochviewer-helper";

/// Set to stop the viewer asking for access at all: every privileged reading is
/// then blank and the window says so. For headless runs and for development.
pub const NO_HELPER_VAR: &str = "ROCHVIEWER_NO_HELPER";

static LOAD_ERROR: OnceLock<Option<String>> = OnceLock::new();

/// Open the best backend this process can have. Called once per process.
pub fn open() -> Arc<dyn Hardware> {
    match open_uncached() {
        Ok(backend) => {
            let _ = LOAD_ERROR.set(None);
            backend
        }
        Err(reason) => {
            let _ = LOAD_ERROR.set(Some(reason.clone()));
            Arc::new(Unavailable(reason))
        }
    }
}

/// Why register access is unavailable, or `None` when it is working. Settles
/// the question first if nothing has asked yet.
pub fn load_error() -> Option<String> {
    if LOAD_ERROR.get().is_none() {
        let _ = super::hw();
    }
    LOAD_ERROR.get().cloned().flatten()
}

fn open_uncached() -> Result<Arc<dyn Hardware>, String> {
    // The window's own tests link this crate without `cfg(test)`, and they must
    // neither touch the machine nor put a password prompt on its screen.
    if cfg!(feature = "testing") {
        return Err("no hardware in unit tests".into());
    }
    let direct_error = match direct::Direct::open() {
        Ok(direct) => return Ok(Arc::new(direct)),
        Err(error) => error,
    };
    // Already root and still refused: another process would be refused the same
    // way, so this reason is the answer.
    if is_root() {
        return Err(direct_error);
    }
    if std::env::var_os(NO_HELPER_VAR).is_some() {
        return Err(format!("Register access is turned off: {NO_HELPER_VAR} is set."));
    }
    proxy::start().map(|proxy| Arc::new(proxy) as Arc<dyn Hardware>)
}

pub fn is_root() -> bool {
    // SAFETY: geteuid takes nothing and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

/// Whether `/dev/i2c-<n>` exists, for an adapter number.
pub fn i2c_device_present(adapter: u32) -> bool {
    std::path::Path::new(&format!("/dev/i2c-{adapter}")).exists()
}

/// The kernel's adapter whose name starts with `prefix`, as its number.
///
/// World-readable, so no helper is involved. The lowest number wins: a
/// multiplexed bus adds its segments after the adapter they hang off.
pub fn find_i2c_adapter(prefix: &str) -> Option<u32> {
    let mut found: Vec<u32> = std::fs::read_dir("/sys/bus/i2c/devices")
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let number: u32 = entry.file_name().to_str()?.strip_prefix("i2c-")?.parse().ok()?;
            let name = std::fs::read_to_string(entry.path().join("name")).ok()?;
            name.starts_with(prefix).then_some(number)
        })
        .collect();
    found.sort_unstable();
    found.first().copied()
}
