//! `cargo xtask install`: the viewer, a launcher entry and its icon, for one user
//! with no root needed. On Windows the program is a single file with nothing to
//! install, so this is Linux only.

use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

/// What cargo calls the program.
const BINARY: &str = "RochViewer";
/// The name it answers to on the command line.
const COMMAND: &str = "rochviewer";
/// Named after the Wayland app id, which is how a compositor finds a running
/// window's entry and icon.
const DESKTOP_FILE: &str = "share/applications/RochViewer.desktop";
const ICON_FILE: &str = "share/icons/hicolor/256x256/apps/RochViewer.png";
const USAGE: &str = "Usage: cargo xtask install|uninstall [--prefix PATH]";

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<()> {
    match args.first().map(String::as_str) {
        None | Some("help" | "--help" | "-h") => {
            println!("Roch Viewer developer tools\n\n  {}", USAGE.trim_start_matches("Usage: "));
            Ok(())
        }
        Some("install") => install(&args[1..]),
        Some("uninstall") => uninstall(&args[1..]),
        Some(other) => Err(format!("Unknown command '{other}'. {USAGE}")),
    }
}

fn prefix_from(args: &[String]) -> Result<PathBuf> {
    match args {
        [] => std::env::var_os("HOME")
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

/// The largest PNG in an .ico file. Every size this project's holds is one.
fn largest_ico_png(data: &[u8]) -> Option<&[u8]> {
    const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
    let count = u16::from_le_bytes([*data.get(4)?, *data.get(5)?]) as usize;
    (0..count)
        .filter_map(|index| {
            let entry = data.get(6 + index * 16..6 + (index + 1) * 16)?;
            // Zero in the directory means 256.
            let width = if entry[0] == 0 { 256 } else { entry[0] as u32 };
            let length = u32::from_le_bytes(entry[8..12].try_into().ok()?) as usize;
            let offset = u32::from_le_bytes(entry[12..16].try_into().ok()?) as usize;
            let blob = data.get(offset..offset.checked_add(length)?)?;
            blob.starts_with(&PNG_MAGIC).then_some((width, blob))
        })
        .max_by_key(|(width, _)| *width)
        .map(|(_, blob)| blob)
}

/// Write next to the target and rename over it. Copying straight onto a running
/// program fails with "text file busy"; a rename leaves the old copy running and
/// the new one in place.
fn put_file(source: &[u8], target: &Path, executable: bool) -> Result<()> {
    let folder = target.parent().ok_or("Invalid install path")?;
    std::fs::create_dir_all(folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let pending = folder.join(format!(".rochviewer-pending-{}", std::process::id()));
    std::fs::write(&pending, source).map_err(|e| format!("{}: {e}", pending.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pending, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    std::fs::rename(&pending, target).map_err(|e| {
        let _ = std::fs::remove_file(&pending);
        format!("{}: {e}", target.display())
    })
}

fn install(args: &[String]) -> Result<()> {
    let prefix = prefix_from(args)?;
    if cfg!(windows) {
        return Err("On Windows there is nothing to install: run target\\release\\RochViewer.exe.".into());
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("Workspace root missing")?;
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let status = Command::new(cargo)
        .args(["build", "--release", "--locked", "-p", "roch-viewer-app"])
        .current_dir(root)
        .status()
        .map_err(|e| e.to_string())?;
    if !status.success() {
        return Err(format!("cargo build failed ({status}); nothing was installed."));
    }

    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let target = if target.is_relative() { root.join(target) } else { target };
    let built = std::fs::read(target.join("release").join(BINARY)).map_err(|e| e.to_string())?;
    let ico = std::fs::read(root.join("icon.ico")).map_err(|e| e.to_string())?;
    let icon = largest_ico_png(&ico).ok_or("icon.ico holds no PNG to install")?;

    let command = prefix.join("bin").join(COMMAND);
    put_file(&built, &command, true)?;
    put_file(desktop_entry(&command).as_bytes(), &prefix.join(DESKTOP_FILE), false)?;
    put_file(icon, &prefix.join(ICON_FILE), false)?;

    println!("Installed {}", command.display());
    println!("Launcher entry and icon are under {}", prefix.join("share").display());
    println!("\nIt asks for access each time it opens. To stop that, once:\n  sudo {} setup", command.display());
    Ok(())
}

fn uninstall(args: &[String]) -> Result<()> {
    let prefix = prefix_from(args)?;
    let command = prefix.join("bin").join(COMMAND);
    for path in [command.clone(), prefix.join(DESKTOP_FILE), prefix.join(ICON_FILE)] {
        match std::fs::remove_file(&path) {
            Ok(()) => println!("Removed {}", path.display()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    println!("If you ran the setup, undo it before this copy is gone, or remove these by hand:");
    println!("  /usr/local/libexec/rochviewer-helper  /etc/modules-load.d/rochviewer.conf");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_commands_fail_before_doing_work() {
        assert!(run(vec!["invalid".into()]).is_err());
        assert!(run(vec!["install".into(), "--invalid".into()]).is_err());
        assert!(run(vec!["uninstall".into(), "--invalid".into()]).is_err());
    }

    #[test]
    fn a_prefix_must_be_absolute_and_flags_are_checked() {
        assert!(prefix_from(&["--prefix".into(), "relative".into()]).is_err());
        assert!(prefix_from(&["--prefix".into()]).is_err());
        assert!(prefix_from(&["--other".into(), "/x".into()]).is_err());
        // What counts as absolute is the platform's business.
        let absolute = if cfg!(windows) { r"C:\opt\roch" } else { "/opt/roch" };
        assert_eq!(prefix_from(&["--prefix".into(), absolute.into()]).unwrap(), PathBuf::from(absolute));
    }

    #[test]
    fn the_entry_points_at_the_installed_command_and_matches_the_app_id() {
        let entry = desktop_entry(Path::new("/home/my user/.local/bin/rochviewer"));
        assert!(entry.starts_with("[Desktop Entry]\n"));
        assert!(entry.contains("\nExec=\"/home/my user/.local/bin/rochviewer\"\n"), "{entry}");
        assert!(entry.contains("\nStartupWMClass=RochViewer\n"));
        assert!(entry.contains("\nIcon=RochViewer\n"));
        assert!(DESKTOP_FILE.ends_with("RochViewer.desktop") && ICON_FILE.ends_with("RochViewer.png"));
    }

    #[test]
    fn the_projects_icon_has_a_png_to_install() {
        let ico = std::fs::read(Path::new(env!("CARGO_MANIFEST_DIR")).join("../icon.ico")).unwrap();
        let png = largest_ico_png(&ico).expect("a PNG entry");
        assert!(png.starts_with(&[0x89, b'P', b'N', b'G']));
    }

    #[test]
    fn a_running_program_can_be_replaced() {
        let dir = std::env::temp_dir().join(format!("rochviewer-install-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let target = dir.join("bin/rochviewer");
        put_file(b"old", &target, true).unwrap();
        put_file(b"new", &target, true).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"new");
        assert_eq!(std::fs::read_dir(dir.join("bin")).unwrap().count(), 1, "nothing left behind");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
