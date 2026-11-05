//! `rochviewer install`: the program, a launcher entry and its icon, for one user
//! with no root needed.
//!
//! Part of the program rather than of the build tools, because a release is a
//! download: whoever unpacks one has no Rust to run `cargo xtask` with, and what
//! this needs is already inside the executable. `cargo xtask install` builds the
//! program and then asks it to do this, so there is one way in.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use rochviewer::hardware::linux::{INSTALLED_HELPER, is_root};

use crate::ui::asset_path::{ICON_ICO, ico_png_entries};

/// The name it answers to on the command line.
const COMMAND: &str = "rochviewer";
/// Named after the Wayland app id, which is how a compositor finds a running
/// window's entry and icon.
const DESKTOP_FILE: &str = "share/applications/RochViewer.desktop";
const ICON_FILE: &str = "share/icons/hicolor/256x256/apps/RochViewer.png";
const USAGE: &str = "Usage: rochviewer install|uninstall [--prefix PATH]";

/// Where to install: `--prefix`, or the user's own `~/.local`. As root that would
/// be `/root`, which is nobody's launcher, so that has to be said out loud.
fn prefix_from(arguments: &[String], home: Option<OsString>, root: bool) -> Result<PathBuf, String> {
    match arguments {
        [] if root => {
            Err("Run this as yourself rather than as root: it installs for one user. For everyone, use --prefix /usr/local.".into())
        }
        [] => home
            .map(|home| PathBuf::from(home).join(".local"))
            .ok_or_else(|| "HOME is not set. Use --prefix PATH.".to_string()),
        [flag, path] if flag == "--prefix" => {
            let path = PathBuf::from(path);
            if path.is_absolute() { Ok(path) } else { Err("--prefix needs an absolute path.".into()) }
        }
        _ => Err(USAGE.into()),
    }
}

/// The launcher entry. Quoted because the spec requires it for anything but plain
/// characters, and a prefix can be any path.
fn desktop_entry(command: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Roch Viewer\nGenericName=Memory timing viewer\n\
         Comment=Memory timings, hardware details and live telemetry\nExec=\"{}\"\nIcon=RochViewer\nTerminal=false\n\
         Categories=System;Monitor;\nStartupWMClass=RochViewer\n",
        command.display()
    )
}

/// The largest picture in the icon file.
fn icon_png() -> Option<&'static [u8]> {
    ico_png_entries(ICON_ICO).into_iter().max_by_key(|(width, _)| *width).map(|(_, png)| png)
}

/// Write next to the target and rename over it. Copying straight onto a running
/// program fails with "text file busy"; a rename leaves the old copy running and
/// the new one in place.
fn put_file(source: &[u8], target: &Path, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let folder = target.parent().ok_or("Invalid install path")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let pending = folder.join(format!(".rochviewer-pending-{}", std::process::id()));
    let finish = || -> std::io::Result<()> {
        std::fs::write(&pending, source)?;
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(if executable { 0o755 } else { 0o644 }))?;
        std::fs::rename(&pending, target)
    };
    finish().map_err(|error| {
        let _ = std::fs::remove_file(&pending);
        format!("{}: {error}", target.display())
    })
}

/// Everything into `prefix`, and the command's path.
fn install_into(prefix: &Path, program: &[u8]) -> Result<PathBuf, String> {
    let icon = icon_png().ok_or("the icon file holds no PNG to install")?;
    let command = prefix.join("bin").join(COMMAND);
    put_file(program, &command, true)?;
    put_file(desktop_entry(&command).as_bytes(), &prefix.join(DESKTOP_FILE), false)?;
    put_file(icon, &prefix.join(ICON_FILE), false)?;
    Ok(command)
}

/// Whatever is there of it in `prefix`, and what was removed.
fn uninstall_from(prefix: &Path) -> Result<Vec<PathBuf>, String> {
    let mut removed = Vec::new();
    for path in [prefix.join("bin").join(COMMAND), prefix.join(DESKTOP_FILE), prefix.join(ICON_FILE)] {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(path),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", path.display())),
        }
    }
    Ok(removed)
}

fn on_path(folder: &Path) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|entry| entry == folder))
}

pub fn install(arguments: &[String]) -> i32 {
    let result = prefix_from(arguments, std::env::var_os("HOME"), is_root()).and_then(|prefix| {
        let exe = std::env::current_exe().map_err(|error| format!("Could not find this program: {error}"))?;
        let program = std::fs::read(&exe).map_err(|error| format!("{}: {error}", exe.display()))?;
        install_into(&prefix, &program).map(|command| (prefix, command))
    });
    let (prefix, command) = match result {
        Ok(installed) => installed,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    println!("Installed {}", command.display());
    println!("Launcher entry and icon are under {}", prefix.join("share").display());
    if command.parent().is_some_and(|folder| !on_path(folder)) {
        println!("Its folder is not on your PATH, so to run it by name, use the full path. The launcher entry works either way.");
    }
    if Path::new(INSTALLED_HELPER).exists() {
        println!("\nYou have run the setup before. Run it again so the helper matches this version:");
    } else {
        println!("\nIt asks for access each time it opens. To stop that, once:");
    }
    println!("  sudo {} setup", command.display());
    0
}

pub fn uninstall(arguments: &[String]) -> i32 {
    let prefix = match prefix_from(arguments, std::env::var_os("HOME"), is_root()) {
        Ok(prefix) => prefix,
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    };
    match uninstall_from(&prefix) {
        Ok(removed) => {
            for path in removed {
                println!("Removed {}", path.display());
            }
        }
        Err(error) => {
            eprintln!("{error}");
            return 1;
        }
    }
    if Path::new(INSTALLED_HELPER).exists() {
        println!("The helper from `rochviewer setup` is still there. To remove it too:");
        println!("  sudo rm {INSTALLED_HELPER} /etc/modules-load.d/rochviewer.conf");
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temporary(name: &str) -> PathBuf {
        let folder = std::env::temp_dir().join(format!("rochviewer-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        folder
    }

    #[test]
    fn the_prefix_is_home_or_an_absolute_path_given() {
        let home = || Some(OsString::from("/home/someone"));
        assert_eq!(prefix_from(&[], home(), false).unwrap(), PathBuf::from("/home/someone/.local"));
        assert!(prefix_from(&[], None, false).is_err());
        assert_eq!(prefix_from(&["--prefix".into(), "/opt/roch".into()], home(), false).unwrap(), PathBuf::from("/opt/roch"));
        assert!(prefix_from(&["--prefix".into(), "relative".into()], home(), false).is_err());
        assert!(prefix_from(&["--prefix".into()], home(), false).is_err());
        assert!(prefix_from(&["--other".into(), "/x".into()], home(), false).is_err());
    }

    #[test]
    fn root_is_not_quietly_installed_into_its_own_home() {
        let home = || Some(OsString::from("/root"));
        assert!(prefix_from(&[], home(), true).unwrap_err().contains("as yourself"));
        assert_eq!(prefix_from(&["--prefix".into(), "/usr/local".into()], home(), true).unwrap(), PathBuf::from("/usr/local"));
    }

    #[test]
    fn the_entry_points_at_the_installed_command_and_matches_the_app_id() {
        let entry = desktop_entry(Path::new("/home/my user/.local/bin/rochviewer"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/home/my user/.local/bin/rochviewer\"\n"), "{entry}");
        assert!(entry.contains("\nStartupWMClass=RochViewer\n"));
        assert!(entry.contains("\nIcon=RochViewer\n"));
        assert!(DESKTOP_FILE.ends_with("RochViewer.desktop") && ICON_FILE.ends_with("RochViewer.png"));
        assert_eq!(crate::ui::admin::APP_ID, "RochViewer", "how a compositor finds the entry");
    }

    #[test]
    fn the_icon_is_a_png_of_the_size_its_folder_says() {
        let png = icon_png().expect("a PNG entry");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
        // The width, big-endian, from the IHDR chunk.
        assert_eq!(u32::from_be_bytes(png[16..20].try_into().unwrap()), 256);
    }

    #[test]
    fn a_running_program_can_be_replaced() {
        let folder = temporary("replace");
        let target = folder.join("bin/rochviewer");
        put_file(b"old", &target, true).unwrap();
        put_file(b"new", &target, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_dir(folder.join("bin")).unwrap().count(), 1, "nothing left behind");
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn install_then_uninstall_leaves_nothing_of_its_own() {
        let prefix = temporary("install");
        let command = install_into(&prefix, b"the program").unwrap();
        assert_eq!(command, prefix.join("bin/rochviewer"));
        assert_eq!(std::fs::read(&command).unwrap(), b"the program");
        assert_eq!(std::fs::metadata(&command).unwrap().permissions().mode() & 0o777, 0o755);
        let entry = std::fs::read_to_string(prefix.join(DESKTOP_FILE)).unwrap();
        assert!(entry.contains(&command.display().to_string()));
        assert_eq!(std::fs::metadata(prefix.join(DESKTOP_FILE)).unwrap().permissions().mode() & 0o777, 0o644);
        assert!(std::fs::read(prefix.join(ICON_FILE)).unwrap().starts_with(&[0x89, b'P', b'N', b'G']));

        assert_eq!(uninstall_from(&prefix).unwrap().len(), 3);
        assert!(!command.exists() && !prefix.join(DESKTOP_FILE).exists() && !prefix.join(ICON_FILE).exists());
        assert_eq!(uninstall_from(&prefix).unwrap(), Vec::<PathBuf>::new(), "nothing there is not an error");
        let _ = std::fs::remove_dir_all(&prefix);
    }
}
