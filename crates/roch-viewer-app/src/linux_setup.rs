//! `rochviewer setup`: stop asking for access at every launch.
//!
//! Without this the viewer starts its helper through `pkexec`, and the desktop
//! asks for a password each time, which is what UAC does on Windows. With it, a
//! root-owned copy of the program sits at [`INSTALLED_HELPER`], executable only by
//! the person who set it up, and carrying the kernel capabilities it needs as file
//! attributes, so starting it asks nothing.
//!
//! Not setuid, because it does not need to be root: only these capabilities, which
//! last only as long as the helper does, and which nothing it starts inherits. And
//! under that file name the program is only ever the helper. What it can do is
//! what `rochviewer::hardware::linux::direct` allows: read device registers and
//! firmware tables, and make the narrow set of selector writes those reads need.
//! It cannot read or write RAM, and it writes nothing to disk.

use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt, chown};
use std::path::Path;

use rochviewer::hardware::linux::{INSTALLED_HELPER, is_root};

/// Makes `/dev/i2c-*` exist after a reboot, for the SMBus readings. A helper with
/// capabilities cannot load the module itself, as one started as root does.
const MODULES_LOAD_FILE: &str = "/etc/modules-load.d/rochviewer.conf";
const MODULES_LOAD_TEXT: &str = "# Roch Viewer reads the memory modules through the kernel's SMBus adapter.\ni2c-dev\n";

// linux/capability.h
const CAP_DAC_OVERRIDE: u32 = 1;
const CAP_DAC_READ_SEARCH: u32 = 2;
const CAP_SYS_RAWIO: u32 = 17;
const CAP_SYS_ADMIN: u32 = 21;
const VFS_CAP_REVISION_2: u32 = 0x0200_0000;
const VFS_CAP_FLAGS_EFFECTIVE: u32 = 0x0000_0001;

/// `/dev/mem` and the ports need RAWIO, and reading the rest of a function's
/// configuration space past 64 bytes needs ADMIN. The two DAC ones are for what
/// root owns: `/dev/mem` and the firmware tables to read, `/dev/i2c-*` to open for
/// the SMBus transfers.
const CAPABILITIES: u32 =
    1 << CAP_DAC_OVERRIDE | 1 << CAP_DAC_READ_SEARCH | 1 << CAP_SYS_RAWIO | 1 << CAP_SYS_ADMIN;

/// `security.capability` as the kernel stores it: `struct vfs_ns_cap_data`'s older
/// sibling, a magic word and a permitted/inheritable pair for each half of the mask.
fn capability_xattr() -> [u8; 20] {
    let words = [VFS_CAP_REVISION_2 | VFS_CAP_FLAGS_EFFECTIVE, CAPABILITIES, 0, 0, 0];
    let mut bytes = [0u8; 20];
    for (index, word) in words.into_iter().enumerate() {
        bytes[index * 4..index * 4 + 4].copy_from_slice(&word.to_le_bytes());
    }
    bytes
}

const USAGE: &str = "\
rochviewer setup            install the helper for the person running sudo
rochviewer setup --status   say whether it is installed, and whether it will work
rochviewer setup --show     what setup would change, without changing it
rochviewer setup --undo     remove it
";

pub fn run(arguments: &[String]) -> i32 {
    match arguments.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] => install(),
        ["--status"] => status(),
        ["--show"] => show(),
        ["--undo"] => undo(),
        _ => {
            eprint!("{USAGE}");
            2
        }
    }
}

fn exe_for_message() -> String {
    std::env::current_exe().map(|exe| exe.display().to_string()).unwrap_or_else(|_| "rochviewer".into())
}

fn require_root(verb: &str) -> bool {
    if is_root() {
        return true;
    }
    // sudo only searches its own short path, which a home install is not on.
    eprintln!("That needs root. Run: sudo {} setup{verb}", exe_for_message());
    false
}

/// The group that may run the helper: that of whoever asked for it.
fn invoking_group() -> Option<u32> {
    if let Some(gid) = std::env::var("SUDO_GID").ok().and_then(|gid| gid.parse().ok()) {
        return Some(gid);
    }
    let uid: u32 = std::env::var("PKEXEC_UID").ok()?.parse().ok()?;
    // SAFETY: getpwuid returns static storage or null, read before anything else
    // here could call it again.
    unsafe {
        let entry = libc::getpwuid(uid);
        (!entry.is_null()).then(|| (*entry).pw_gid)
    }
}

fn show() -> i32 {
    println!("Setup would:");
    println!("  copy this program to {INSTALLED_HELPER}, owned by root and executable only by your group");
    println!("  give that copy the capabilities cap_dac_override, cap_dac_read_search, cap_sys_rawio and cap_sys_admin");
    println!("  write {MODULES_LOAD_FILE}, so /dev/i2c-* exists after a reboot, and load i2c-dev now");
    0
}

fn install() -> i32 {
    if !require_root("") {
        return 1;
    }
    let Some(group) = invoking_group() else {
        eprintln!("Could not tell who this is for: run it through sudo or pkexec rather than as root directly.");
        return 1;
    };
    match install_helper(group) {
        Ok(()) => {
            println!("Installed {INSTALLED_HELPER}. The viewer will no longer ask for access when it opens.");
            println!("Run this again after updating the viewer: `rochviewer setup --undo` removes it.");
            0
        }
        Err(error) => {
            eprintln!("Setup failed: {error}");
            1
        }
    }
}

fn install_helper(group: u32) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|error| format!("could not find this program: {error}"))?;
    let target = Path::new(INSTALLED_HELPER);
    let folder = target.parent().ok_or("the helper's path has no folder")?;
    std::fs::create_dir_all(folder).map_err(|error| format!("{}: {error}", folder.display()))?;

    // A temporary name in the same folder, so what is there is always whole. The
    // capabilities go on last: a file that gets them is one nobody can still write.
    let temporary = folder.join(".rochviewer-helper.new");
    let _ = std::fs::remove_file(&temporary);
    std::fs::copy(&exe, &temporary).map_err(|error| format!("copying {}: {error}", exe.display()))?;
    let finish = || -> Result<(), String> {
        chown(&temporary, Some(0), Some(group)).map_err(|error| format!("chown: {error}"))?;
        std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o750))
            .map_err(|error| format!("chmod: {error}"))?;
        set_capabilities(&temporary)?;
        std::fs::rename(&temporary, target).map_err(|error| format!("rename: {error}"))
    };
    if let Err(error) = finish() {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    std::fs::write(MODULES_LOAD_FILE, MODULES_LOAD_TEXT).map_err(|error| format!("{MODULES_LOAD_FILE}: {error}"))?;
    let _ = std::process::Command::new("modprobe").args(["-q", "i2c-dev"]).status();
    Ok(())
}

fn c_path(path: &Path) -> Result<CString, String> {
    CString::new(path.as_os_str().as_bytes()).map_err(|_| format!("{} has a NUL in it", path.display()))
}

fn set_capabilities(path: &Path) -> Result<(), String> {
    let path_text = c_path(path)?;
    let value = capability_xattr();
    // SAFETY: both strings are NUL-terminated, and the value's length is passed.
    let status = unsafe {
        libc::setxattr(path_text.as_ptr(), c"security.capability".as_ptr(), value.as_ptr().cast(), value.len(), 0)
    };
    if status != 0 {
        return Err(format!("setting its capabilities: {}", std::io::Error::last_os_error()));
    }
    Ok(())
}

fn has_capabilities(path: &Path) -> bool {
    let Ok(path_text) = c_path(path) else { return false };
    let mut value = [0u8; 20];
    // SAFETY: the buffer's length is passed, and the kernel writes at most that.
    let length = unsafe {
        libc::getxattr(path_text.as_ptr(), c"security.capability".as_ptr(), value.as_mut_ptr().cast(), value.len())
    };
    length == value.len() as isize && value == capability_xattr()
}

fn status() -> i32 {
    let helper = Path::new(INSTALLED_HELPER);
    match std::fs::metadata(helper) {
        Err(_) => println!("Helper: not installed. The viewer asks for access each time it opens."),
        Ok(metadata) => {
            let capabilities = if has_capabilities(helper) { "with its capabilities" } else { "WITHOUT its capabilities" };
            println!(
                "Helper: {INSTALLED_HELPER}, {capabilities}, mode {:o}, group {}.",
                metadata.mode() & 0o7777,
                metadata.gid()
            );
        }
    }
    let i2c = Path::new("/sys/class/i2c-dev").exists();
    println!(
        "SMBus: {}",
        if i2c { "i2c-dev is loaded." } else { "i2c-dev is not loaded, so the raw transport is used for the module rows." }
    );
    let lockdown = std::fs::read_to_string("/sys/kernel/security/lockdown").unwrap_or_default();
    if rochviewer::hardware::linux::direct::lockdown_is_active(&lockdown) {
        println!("Lockdown: {}. Register access is blocked, whatever is set up here.", lockdown.trim());
    }
    // Saying what would happen rather than trying it: a question about the setup
    // must not put a password prompt on the screen.
    println!(
        "Access: {}",
        match rochviewer::hardware::linux::direct::Direct::open() {
            Ok(_) => "this process has it directly.".to_string(),
            Err(_) if helper.exists() => "through the helper, without asking.".to_string(),
            Err(_) => "the viewer will ask for it when it opens, through pkexec.".to_string(),
        }
    );
    0
}

fn undo() -> i32 {
    if !require_root(" --undo") {
        return 1;
    }
    for path in [INSTALLED_HELPER, MODULES_LOAD_FILE] {
        match std::fs::remove_file(path) {
            Ok(()) => println!("Removed {path}."),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                eprintln!("Could not remove {path}: {error}");
                return 1;
            }
        }
    }
    println!("The viewer will ask for access when it opens.");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capabilities_are_written_as_the_kernel_stores_them() {
        // What `setcap cap_dac_override,cap_dac_read_search,cap_sys_rawio,cap_sys_admin+ep`
        // writes: revision 2 with the effective flag, the low half's mask, nothing
        // inheritable, nothing in the high half.
        let bytes = capability_xattr();
        assert_eq!(&bytes[..4], &0x0200_0001u32.to_le_bytes());
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), (1 << 1) | (1 << 2) | (1 << 17) | (1 << 21));
        assert_eq!(&bytes[8..], &[0; 12]);
    }
}
