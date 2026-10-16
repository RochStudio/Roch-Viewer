//! Which DDR generation is installed, from what the firmware already says.
//!
//! SMBIOS and WMI, with no register read anywhere -- which is why this lives
//! here rather than in a platform backend. Both platforms need the answer and
//! neither needs a memory controller to get it.

use crate::memo::Memo;
use crate::wmi_util::{WmiObject, static_class};

const SMBIOS_DDR4: [i64; 2] = [26, 30];
const SMBIOS_DDR5: [i64; 2] = [34, 35];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DdrGeneration {
    Ddr4,
    Ddr5,
    Unknown,
}

impl DdrGeneration {
    pub fn as_str(self) -> &'static str {
        match self {
            DdrGeneration::Ddr4 => "DDR4",
            DdrGeneration::Ddr5 => "DDR5",
            DdrGeneration::Unknown => "Unknown",
        }
    }
}

impl std::fmt::Display for DdrGeneration {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// DDR4, DDR5 or Unknown from SMBIOS types, with text fallbacks for systems
/// that do not populate `SMBIOSMemoryType` correctly.
pub fn detect_from(memories: &[WmiObject], boards: &[WmiObject]) -> DdrGeneration {
    let mut ddr4 = false;
    let mut ddr5 = false;
    for memory in memories {
        for field in ["SMBIOSMemoryType", "MemoryType"] {
            if let Some(code) = memory.int(field) {
                if SMBIOS_DDR4.contains(&code) {
                    ddr4 = true;
                } else if SMBIOS_DDR5.contains(&code) {
                    ddr5 = true;
                }
            }
        }
    }
    if ddr5 {
        return DdrGeneration::Ddr5;
    }
    if ddr4 {
        return DdrGeneration::Ddr4;
    }

    let mut text_fields = Vec::new();
    for memory in memories {
        for field in ["PartNumber", "Description", "Caption"] {
            let value = memory.str_or_empty(field).to_uppercase();
            if !value.is_empty() {
                text_fields.push(value);
            }
        }
    }
    for board in boards {
        text_fields.push(board.str_or_empty("Product").to_uppercase());
        text_fields.push(board.str_or_empty("Version").to_uppercase());
    }
    let joined = text_fields.join(" ");
    if joined.contains("DDR5") {
        DdrGeneration::Ddr5
    } else if joined.contains("DDR4") {
        DdrGeneration::Ddr4
    } else {
        DdrGeneration::Unknown
    }
}

static DETECTED: Memo<DdrGeneration> = Memo::new();

/// The installed generation, detected once: it cannot change while the
/// machine is running, and the tables ask about twenty times.
pub fn detect_ddr_generation() -> DdrGeneration {
    DETECTED.get_or_init(|| {
        detect_from(&static_class("Win32_PhysicalMemory"), &static_class("Win32_BaseBoard"))
    })
}

pub fn clear_cache() {
    DETECTED.clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi_util::FakeWmi;

    #[test]
    fn smbios_type_wins_and_ddr5_outranks_ddr4() {
        let memories = vec![
            WmiObject::new().with("SMBIOSMemoryType", 26i64),
            WmiObject::new().with("SMBIOSMemoryType", 34i64),
        ];
        assert_eq!(detect_from(&memories, &[]), DdrGeneration::Ddr5);
        assert_eq!(detect_from(&memories[..1], &[]), DdrGeneration::Ddr4);
    }

    #[test]
    fn text_fallback_reads_part_numbers_and_board() {
        let memories = vec![WmiObject::new().with("SMBIOSMemoryType", 0i64).with("PartNumber", "x")];
        let boards = vec![WmiObject::new().with("Product", "PRO Z790-P WIFI DDR4")];
        assert_eq!(detect_from(&memories, &boards), DdrGeneration::Ddr4);
        assert_eq!(detect_from(&[], &[]), DdrGeneration::Unknown);
    }

    #[test]
    fn detection_is_cached() {
        FakeWmi::new()
            .class("Win32_PhysicalMemory", vec![WmiObject::new().with("SMBIOSMemoryType", 34i64)])
            .install();
        assert_eq!(detect_ddr_generation(), DdrGeneration::Ddr5);
        FakeWmi::new().install();
        assert_eq!(detect_ddr_generation(), DdrGeneration::Ddr5);
        clear_cache();
        assert_eq!(detect_ddr_generation(), DdrGeneration::Unknown);
    }
}
