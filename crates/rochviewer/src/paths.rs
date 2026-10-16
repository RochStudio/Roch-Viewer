//! Directories a file that ships with the project might be in.
//!
//! Two things are looked up this way -- the low-level driver and the icon --
//! and they differ in where a bundled copy ranks, so each builds its own
//! order. What they share is this: the executable's own directory is what the
//! user can see and drop a file into, and when running from a checkout
//! (`cargo run`) the project root is a few levels above the executable.

use std::path::{Path, PathBuf};

/// How many directories to look at, starting from the executable's own:
/// target/<profile>/ -> target/ -> project root, plus one for luck.
pub const PARENT_LEVELS: usize = 4;

/// The executable's own directory.
pub fn exe_directory() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
}

/// A directory, then its parents, nearest first, at most `levels` long.
pub fn parent_chain(start: &Path, levels: usize) -> Vec<PathBuf> {
    let mut chain = Vec::new();
    let mut current = Some(start.to_path_buf());
    while let Some(dir) = current {
        if chain.len() >= levels {
            break;
        }
        current = dir.parent().map(Path::to_path_buf);
        chain.push(dir);
    }
    chain
}

/// The executable's directory and the ones above it, nearest first. The
/// project root is in here when running from `target/debug` or
/// `target/release`, so a driver placed beside the README is found.
pub fn exe_chain() -> Vec<PathBuf> {
    exe_directory()
        .map(|dir| parent_chain(&dir, PARENT_LEVELS))
        .unwrap_or_default()
}

/// The same list with blanks and repeats dropped, order kept.
pub fn dedupe(directories: impl IntoIterator<Item = PathBuf>) -> Vec<PathBuf> {
    let mut ordered: Vec<PathBuf> = Vec::new();
    for directory in directories {
        if directory.as_os_str().is_empty() || ordered.contains(&directory) {
            continue;
        }
        ordered.push(directory);
    }
    ordered
}

/// The first directory in `directories` holding `name`, joined to it.
pub fn find_in(directories: &[PathBuf], name: &str) -> Option<PathBuf> {
    directories
        .iter()
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_is_nearest_first_and_bounded() {
        let chain = parent_chain(Path::new(r"C:\a\b\c\d\e"), 3);
        assert_eq!(
            chain,
            vec![
                PathBuf::from(r"C:\a\b\c\d\e"),
                PathBuf::from(r"C:\a\b\c\d"),
                PathBuf::from(r"C:\a\b\c"),
            ]
        );
    }

    #[test]
    fn chain_stops_at_the_root() {
        let chain = parent_chain(Path::new("C:\\"), 5);
        assert_eq!(chain, vec![PathBuf::from("C:\\")]);
    }

    #[test]
    fn dedupe_keeps_order() {
        let out = dedupe(vec![
            PathBuf::from("a"),
            PathBuf::from(""),
            PathBuf::from("b"),
            PathBuf::from("a"),
        ]);
        assert_eq!(out, vec![PathBuf::from("a"), PathBuf::from("b")]);
    }
}
