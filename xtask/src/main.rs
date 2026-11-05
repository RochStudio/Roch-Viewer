//! `cargo xtask install`: build the viewer and have it install itself, for one
//! user with no root needed. On Windows the program is a single file with nothing
//! to install, so this is Linux only.
//!
//! The installing is the program's own job (`rochviewer install`), because someone
//! who downloaded a release has no Rust to run this with.

use std::path::{Path, PathBuf};
use std::process::Command;

type Result<T> = std::result::Result<T, String>;

/// What cargo calls the program.
const BINARY: &str = "RochViewer";
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
        Some(verb @ ("install" | "uninstall")) => through_the_program(verb, &args[1..]),
        Some(other) => Err(format!("Unknown command '{other}'. {USAGE}")),
    }
}

/// The arguments the program will be given, checked before anything is built.
fn check_arguments(args: &[String]) -> Result<()> {
    match args {
        [] => Ok(()),
        [flag, path] if flag == "--prefix" => {
            if Path::new(path).is_absolute() { Ok(()) } else { Err("--prefix needs an absolute path.".into()) }
        }
        _ => Err(USAGE.into()),
    }
}

/// Build a release, and run it with this verb.
fn through_the_program(verb: &str, args: &[String]) -> Result<()> {
    check_arguments(args)?;
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
        return Err(format!("cargo build failed ({status}); nothing was changed."));
    }
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| root.join("target"));
    let target = if target.is_relative() { root.join(target) } else { target };
    let status = Command::new(target.join("release").join(BINARY)).arg(verb).args(args).status().map_err(|e| e.to_string())?;
    if status.success() { Ok(()) } else { Err(format!("{verb} failed ({status}).")) }
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
        assert!(check_arguments(&["--prefix".into(), "relative".into()]).is_err());
        assert!(check_arguments(&["--prefix".into()]).is_err());
        assert!(check_arguments(&["--other".into(), "/x".into()]).is_err());
        assert!(check_arguments(&[]).is_ok());
        // What counts as absolute is the platform's business.
        let absolute = if cfg!(windows) { r"C:\opt\roch" } else { "/opt/roch" };
        assert!(check_arguments(&["--prefix".into(), absolute.into()]).is_ok());
    }
}
