//! The one place the product name and version are written.
//!
//! Both are shown in the window title and stamped into the executable's file
//! properties, so a build can be identified from the binary alone without the
//! directory it came from. The number itself comes from the workspace
//! `Cargo.toml`, so the crate version and the displayed version cannot drift.

pub const APP_NAME: &str = "Roch Viewer";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The four-part file version Windows shows: major, minor, patch, 0.
pub fn version_tuple() -> [u16; 4] {
    let mut parts = [0u16; 4];
    for (slot, part) in parts.iter_mut().zip(VERSION.split('.')) {
        *slot = part.parse().unwrap_or(0);
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_three_numbers_and_a_zero() {
        let parts: Vec<&str> = VERSION.split('.').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.parse::<u16>().is_ok()));
        assert_eq!(version_tuple()[3], 0);
    }
}
