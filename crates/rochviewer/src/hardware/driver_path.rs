//! Where to find the low-level access driver.
//!
//! The driver is not distributed with this project -- see
//! THIRD_PARTY_NOTICES -- so it is wherever the person running the tool put
//! it. That is beside the executable for a release build, which is the
//! directory a user can actually see and drop a file into; when running from
//! a checkout, the project root above `target/` is searched as well, which is
//! where the README says to put it.

use std::path::PathBuf;

use crate::paths::{dedupe, exe_chain, find_in};

pub const DLL_NAME: &str = "inpoutx64.dll";

/// Every directory the driver might be in, nearest first.
pub fn search_directories() -> Vec<PathBuf> {
    let mut directories = exe_chain();
    if let Ok(cwd) = std::env::current_dir() {
        directories.push(cwd);
    }
    dedupe(directories)
}

/// The driver's full path, or `None` when it is not in any of them.
pub fn find_driver() -> Option<PathBuf> {
    find_driver_named(DLL_NAME)
}

pub fn find_driver_named(name: &str) -> Option<PathBuf> {
    find_in(&search_directories(), name)
}

/// What to tell someone who has not put the driver anywhere yet.
pub fn missing_message() -> String {
    missing_message_for(DLL_NAME, &search_directories())
}

pub fn missing_message_for(name: &str, directories: &[PathBuf]) -> String {
    let listed: Vec<String> = directories
        .iter()
        .map(|dir| dir.display().to_string())
        .collect();
    format!(
        "{name} not found. It is a third-party component and is not distributed \
         with Roch Viewer; download it from www.highrez.co.uk and put it in \
         one of:\n  {}",
        listed.join("\n  ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_names_the_dll_the_source_and_every_directory() {
        let dirs = vec![PathBuf::from(r"C:\one"), PathBuf::from(r"C:\two")];
        let text = missing_message_for(DLL_NAME, &dirs);
        assert!(text.starts_with("inpoutx64.dll not found."));
        assert!(text.contains("www.highrez.co.uk"));
        assert!(text.contains("\n  C:\\one\n  C:\\two"));
    }

    #[test]
    fn executable_directory_is_searched_first() {
        let dirs = search_directories();
        assert_eq!(dirs.first(), crate::paths::exe_directory().as_ref());
    }
}
