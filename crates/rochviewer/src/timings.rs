//! Platform timing-backend dispatcher.
//!
//! Only the selected platform's table is built. This is the safety boundary
//! that prevents Intel MCHBAR readers from running on AMD hardware: building
//! a table runs its readers, so a table for the wrong platform is never
//! constructed at all.

use crate::display_values::{self, EM_DASH, NOT_AVAILABLE};
use crate::platform_profiles::{Platform, active_platform};
use crate::row::{Channel, Formula, Row};

/// A platform's table and how it words a missing reading.
pub struct Backend {
    pub platform: Platform,
    pub rows: Vec<Row>,
    /// What a raw read that did not work shows as: "N/A" on Intel, an em
    /// dash on AMD and the unsupported profile, as the Python had it.
    pub unavailable: &'static str,
}

impl Backend {
    pub fn apply_formula(&self, raw: Option<u64>, formula: Option<&Formula>) -> String {
        display_values::apply_formula(raw, formula, self.unavailable)
    }

    /// A row as the window shows it.
    pub fn read_value(&self, row: &Row, selected: Option<Channel>) -> String {
        display_values::read_value(row, selected, self.unavailable)
    }

    pub fn row(&self, name: &str) -> Option<&Row> {
        self.rows.iter().find(|row| row.name == name)
    }
}

pub fn load_timing_backend(platform: Platform) -> Backend {
    match platform {
        Platform::Am5 => Backend {
            platform,
            rows: crate::amd::profile::build_timings(),
            unavailable: EM_DASH,
        },
        Platform::Lga1700Ddr4 | Platform::Lga1700Ddr5 | Platform::Lga1851 => Backend {
            platform,
            rows: crate::intel::intel_timings::build_timings(platform),
            unavailable: NOT_AVAILABLE,
        },
        Platform::Unsupported => Backend {
            platform,
            rows: crate::unsupported_profile::build_timings(),
            unavailable: EM_DASH,
        },
    }
}

/// The backend for the machine this runs on.
pub fn load_active_backend() -> Backend {
    load_timing_backend(active_platform())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsupported_backend_is_neutral() {
        let backend = load_timing_backend(Platform::Unsupported);
        assert_eq!(backend.rows.len(), 2);
        assert_eq!(backend.read_value(backend.row("Platform").unwrap(), None), "Unsupported");
        assert_eq!(backend.apply_formula(None, None), EM_DASH);
    }
}
