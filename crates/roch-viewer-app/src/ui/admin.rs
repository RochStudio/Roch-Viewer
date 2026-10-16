//! Elevation and the taskbar identity.

use windows::Win32::UI::Shell::{IsUserAnAdmin, SetCurrentProcessExplicitAppUserModelID, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, w};

/// Windows groups taskbar buttons by Application User Model ID, and a
/// process that never sets one inherits its host's. Any distinct string
/// works; this one is the conventional dotted form, and the same one the
/// Python build used, so pinned buttons carry over.
pub const APP_USER_MODEL_ID: &str = "RochStudio.RochViewer";

/// Whether the current process has administrative privileges.
pub fn is_admin() -> bool {
    // SAFETY: no arguments; returns a BOOL.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Relaunch this executable elevated, with the same arguments, then exit.
///
/// Every register-backed reading needs the driver, and the driver needs an
/// elevated process; a non-elevated window would be a page of blanks.
pub fn run_as_admin() -> ! {
    let exe = std::env::current_exe().unwrap_or_default();
    let args: Vec<String> = std::env::args().skip(1).map(|a| format!("\"{a}\"")).collect();
    let parameters = args.join(" ");
    let exe = HSTRING::from(exe.as_os_str());
    let parameters = HSTRING::from(parameters);
    // SAFETY: plain ShellExecuteW with owned, NUL-terminated strings.
    unsafe {
        ShellExecuteW(
            None,
            w!("runas"),
            &exe,
            if parameters.is_empty() { PCWSTR::null() } else { PCWSTR(parameters.as_ptr()) },
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
    std::process::exit(0);
}

/// Take the taskbar button away from whatever launched us. Failing is not
/// fatal: the button then keeps the host's icon.
pub fn claim_taskbar_identity() {
    // SAFETY: sets a process-wide string; no pointers retained.
    if let Err(error) = unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(APP_USER_MODEL_ID)) } {
        eprintln!("Error claiming taskbar identity: {error}");
    }
}

/// Open a URL or folder with the shell's default handler.
pub fn shell_open(target: &str) {
    // SAFETY: plain ShellExecuteW with owned, NUL-terminated strings.
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

/// Show a file selected in Explorer.
pub fn reveal_in_explorer(path: &std::path::Path) {
    let _ = std::process::Command::new("explorer")
        .arg(format!("/select,{}", path.display()))
        .spawn();
}
