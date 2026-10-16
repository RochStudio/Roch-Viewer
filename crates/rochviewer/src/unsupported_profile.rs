//! Safe neutral profile used when platform detection is inconclusive.

use crate::row::Row;

pub const EM_DASH: &str = "—";

pub fn build_timings() -> Vec<Row> {
    vec![
        Row::new("Platform", "General", "System Info", "Left").with_value("Unsupported"),
        Row::new("Read Status", "General", "System Info", "Left")
            .with_value("Unsupported platform — privileged timing reads disabled"),
    ]
}
