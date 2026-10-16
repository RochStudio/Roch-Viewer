//! Port of rochviewer/intel/intel_timings.py -- in progress.
//!
//! Only the pieces other modules need before the full table is ported live
//! here so far; their signatures are part of the contract and must be kept.

use crate::platform_profiles::Platform;
use crate::row::Row;
use crate::wmi_util::static_class;

/// The full Intel table for `platform`. Not yet ported: returns no rows.
pub fn build_timings(_platform: Platform) -> Vec<Row> {
    Vec::new()
}

/// Board maker and product on one line -- "ASUSTeK COMPUTER INC. PRIME
/// Z790-A WIFI" -- or "Unknown". The revision has its own System Info row.
pub fn get_motherboard_display() -> String {
    let boards = static_class("Win32_BaseBoard");
    let Some(board) = boards.first() else {
        return "Unknown".into();
    };
    let parts: Vec<String> = ["Manufacturer", "Product"]
        .iter()
        .map(|field| board.str_or_empty(field).trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    let joined = parts.join(" ");
    if joined.trim().is_empty() { "Unknown".into() } else { joined.trim().to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi_util::{FakeWmi, WmiObject};

    #[test]
    fn motherboard_display_joins_maker_and_product() {
        FakeWmi::new()
            .class(
                "Win32_BaseBoard",
                vec![WmiObject::new()
                    .with("Manufacturer", "ASUSTeK COMPUTER INC. ")
                    .with("Product", "PRIME Z790-A WIFI")
                    .with("Version", "Rev 1.xx")],
            )
            .install();
        assert_eq!(get_motherboard_display(), "ASUSTeK COMPUTER INC. PRIME Z790-A WIFI");
    }

    #[test]
    fn motherboard_display_unknown_without_wmi() {
        FakeWmi::new().install();
        assert_eq!(get_motherboard_display(), "Unknown");
    }
}
