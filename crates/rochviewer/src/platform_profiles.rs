//! Early platform classification.
//!
//! This module intentionally performs no privileged hardware access. The
//! result is selected before a platform timing backend is built, so AMD
//! systems can never fall through to Intel MCHBAR reads.

use std::sync::LazyLock;

use regex::Regex;

use crate::memo::Memo;
use crate::wmi_util::{self, WmiValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    Lga1700Ddr4,
    Lga1700Ddr5,
    Lga1851,
    Am5,
    Unsupported,
}

pub use Platform::{Am5 as AM5, Lga1700Ddr4 as LGA1700_DDR4, Lga1700Ddr5 as LGA1700_DDR5, Lga1851 as LGA1851, Unsupported as UNSUPPORTED};

impl Platform {
    /// The identifier the Python used, which settings and reports carry.
    pub fn id(self) -> &'static str {
        match self {
            Platform::Lga1700Ddr4 => "lga1700-ddr4",
            Platform::Lga1700Ddr5 => "lga1700-ddr5",
            Platform::Lga1851 => "lga1851",
            Platform::Am5 => "am5",
            Platform::Unsupported => "unsupported",
        }
    }

    pub fn is_intel(self) -> bool {
        matches!(self, Platform::Lga1700Ddr4 | Platform::Lga1700Ddr5 | Platform::Lga1851)
    }

    pub fn is_ddr5(self) -> bool {
        matches!(self, Platform::Lga1700Ddr5 | Platform::Lga1851 | Platform::Am5)
    }
}

pub const DDR4_SMBIOS_TYPE: i64 = 26;
pub const DDR5_SMBIOS_TYPE: i64 = 34;

static RYZEN_MODEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"ryzen\s+(?:[3579]\s+)?(?:pro\s+)?(\d{4})([a-z0-9]*)").unwrap());
static LGA1700_MODEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"core(?:\(tm\))?\s+i[3579]-?(\d{5})([a-z]*)\b").unwrap());
static LGA1851_MODEL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"core(?:\(tm\))?\s+ultra\s+[579]\s+(\d{3})([a-z]*)\b").unwrap());

fn normalise(text: &str) -> String {
    text.trim().to_lowercase()
}

fn ryzen_model(name: &str) -> Option<(String, String)> {
    RYZEN_MODEL
        .captures(name)
        .map(|c| (c[1].to_string(), c[2].to_string()))
}

fn is_desktop_am5_cpu(cpu_name: &str) -> bool {
    let name = normalise(cpu_name);
    if !name.contains("ryzen") || name.contains("threadripper") {
        return false;
    }
    let Some((model, suffix)) = ryzen_model(&name) else {
        return false;
    };
    if !model.starts_with(['7', '9']) {
        return false;
    }
    // Ryzen 8000 desktop parts are Phoenix APUs (including 8700F) and do not
    // inherit the validated desktop-IOD UMC profile. G/GE parts in any later
    // family remain unsupported until their layout is independently verified.
    // Mobile HS/HX/H/U parts likewise use different layouts.
    matches!(suffix.as_str(), "" | "x" | "x3d" | "f" | "xt")
}

/// Whether the non-privileged CPU name identifies desktop Ryzen 9000.
pub fn is_granite_ridge_cpu(cpu_name: &str) -> bool {
    is_desktop_am5_cpu(cpu_name)
        && ryzen_model(&normalise(cpu_name)).is_some_and(|(model, _)| model.starts_with('9'))
}

/// Whether the non-privileged CPU name identifies desktop Ryzen 7000.
///
/// Raphael shares Granite Ridge's IO die, SMU mailbox and command numbers,
/// but not its APOB training-record layout or its PM-table versions. Every
/// reading taken on it through those paths is from ZenStates-Core's Zen 4
/// tables and has not been confirmed on Raphael hardware by this project, so
/// callers mark it unverified.
pub fn is_raphael_cpu(cpu_name: &str) -> bool {
    is_desktop_am5_cpu(cpu_name)
        && ryzen_model(&normalise(cpu_name)).is_some_and(|(model, _)| model.starts_with('7'))
}

fn is_lga1700_desktop_cpu(cpu_name: &str) -> bool {
    let name = normalise(cpu_name);
    let Some(c) = LGA1700_MODEL.captures(&name) else {
        return false;
    };
    if !matches!(&c[1][..2], "12" | "13" | "14") {
        return false;
    }
    matches!(&c[2], "" | "f" | "k" | "kf" | "ks" | "t")
}

fn is_lga1851_desktop_cpu(cpu_name: &str) -> bool {
    let name = normalise(cpu_name);
    let Some(c) = LGA1851_MODEL.captures(&name) else {
        return false;
    };
    if !c[1].starts_with('2') {
        return false;
    }
    matches!(&c[2], "" | "f" | "k" | "kf" | "ks" | "t")
}

/// Classify from non-privileged WMI/SMBIOS strings and memory types.
pub fn classify_platform(
    manufacturer: &str,
    cpu_name: &str,
    memory_types: &[i64],
    _board_product: &str,
) -> Platform {
    let vendor = normalise(manufacturer);
    let is_amd = vendor.contains("authenticamd") || vendor == "amd" || vendor.contains("advanced micro devices");
    let is_intel = vendor.contains("genuineintel") || vendor.contains("intel");
    let has = |kind: i64| memory_types.contains(&kind);

    if is_amd {
        // Desktop AM5 is DDR5-only. Requiring both Ryzen identity and DDR5
        // prevents an AM4 system from ever reaching the AM5 SMN decoder.
        if is_desktop_am5_cpu(cpu_name) && has(DDR5_SMBIOS_TYPE) {
            return Platform::Am5;
        }
        return Platform::Unsupported;
    }
    if is_intel {
        if has(DDR5_SMBIOS_TYPE) && is_lga1851_desktop_cpu(cpu_name) {
            return Platform::Lga1851;
        }
        if has(DDR4_SMBIOS_TYPE) && is_lga1700_desktop_cpu(cpu_name) {
            return Platform::Lga1700Ddr4;
        }
        if has(DDR5_SMBIOS_TYPE) && is_lga1700_desktop_cpu(cpu_name) {
            return Platform::Lga1700Ddr5;
        }
    }
    Platform::Unsupported
}

/// Python truthiness for a WMI value: `x or y` falls through on these.
fn is_truthy(value: &WmiValue) -> bool {
    match value {
        WmiValue::Null => false,
        WmiValue::Bool(b) => *b,
        WmiValue::Int(i) => *i != 0,
        WmiValue::UInt(u) => *u != 0,
        WmiValue::Float(f) => *f != 0.0,
        WmiValue::Str(s) => !s.is_empty(),
        WmiValue::Array(items) => !items.is_empty(),
    }
}

/// The current platform, using only WMI/SMBIOS metadata.
pub fn detect_current_platform() -> Platform {
    // The process-wide cached copies: these classes describe installed
    // hardware, and the table builders ask for the same three moments later.
    // An unreachable WMI reads as empty, which classifies as unsupported --
    // the answer the Python gave when the query raised.
    let processors = wmi_util::static_class("Win32_Processor");
    let memories = wmi_util::static_class("Win32_PhysicalMemory");
    let boards = wmi_util::static_class("Win32_BaseBoard");
    let processor = processors.first();
    let board = boards.first();
    // `SMBIOSMemoryType or MemoryType`: a zero SMBIOS type falls through. One
    // value that is not a number voids the whole set, as int() raising did.
    let mut types = Vec::new();
    for memory in memories.iter() {
        let raw = memory
            .get("SMBIOSMemoryType")
            .filter(|v| is_truthy(v))
            .or_else(|| memory.get("MemoryType"));
        match raw {
            None | Some(WmiValue::Null) => {}
            Some(value) => match value.to_i64() {
                Some(code) => types.push(code),
                None => {
                    types.clear();
                    break;
                }
            },
        }
    }
    classify_platform(
        &processor.map(|p| p.str_or_empty("Manufacturer")).unwrap_or_default(),
        &processor.map(|p| p.str_or_empty("Name")).unwrap_or_default(),
        &types,
        &board.map(|b| b.str_or_empty("Product")).unwrap_or_default(),
    )
}

static ACTIVE: Memo<Platform> = Memo::new();

/// The platform this process runs on, detected once.
pub fn active_platform() -> Platform {
    ACTIVE.get_or_init(detect_current_platform)
}

/// Pin the active platform (tests, and the table builders' own tests).
pub fn set_active_platform(platform: Platform) {
    ACTIVE.set(platform);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wmi_util::{FakeWmi, WmiObject};

    #[test]
    fn granite_ridge_identity_is_separate_from_raphael() {
        assert!(is_granite_ridge_cpu("AMD Ryzen 7 9850X3D"));
        assert!(is_granite_ridge_cpu("AMD Ryzen 9 9950X"));
        assert!(!is_granite_ridge_cpu("AMD Ryzen 7 7800X3D"));
        assert!(!is_granite_ridge_cpu("AMD Ryzen 7 8700G"));
        assert!(is_raphael_cpu("AMD Ryzen 7 7800X3D"));
        assert!(!is_raphael_cpu("AMD Ryzen 9 9950X"));
    }

    #[test]
    fn b850_granite_ridge_is_am5() {
        assert_eq!(
            classify_platform(
                "AuthenticAMD",
                "AMD Ryzen 7 9850X3D 8-Core Processor",
                &[34, 34],
                "MSI B850MPOWER (MS-7E83)"
            ),
            Platform::Am5
        );
    }

    #[test]
    fn amd_ddr4_is_not_am5() {
        assert_eq!(classify_platform("AuthenticAMD", "Ryzen 9 5950X", &[26], "X570"), Platform::Unsupported);
    }

    #[test]
    fn mobile_ryzen_ddr5_is_not_desktop_am5_profile() {
        for cpu in [
            "AMD Ryzen 7 7840HS with Radeon 780M Graphics",
            "AMD Ryzen 9 7945HX",
            "AMD Ryzen 7 7840U",
        ] {
            assert_eq!(classify_platform("AuthenticAMD", cpu, &[34], "Laptop"), Platform::Unsupported, "{cpu}");
        }
    }

    #[test]
    fn known_desktop_am5_suffixes_are_accepted() {
        for cpu in ["AMD Ryzen 7 7800X3D", "AMD Ryzen 7 9700X", "AMD Ryzen 5 7500F", "AMD Ryzen 7 7700"] {
            assert_eq!(classify_platform("AuthenticAMD", cpu, &[34], "AM5"), Platform::Am5, "{cpu}");
        }
    }

    #[test]
    fn phoenix_and_unvalidated_desktop_apus_are_unsupported() {
        for cpu in ["AMD Ryzen 7 8700G", "AMD Ryzen 5 8500G", "AMD Ryzen 7 8700F", "AMD Ryzen 5 9600G"] {
            assert_eq!(classify_platform("AuthenticAMD", cpu, &[34], "AM5"), Platform::Unsupported, "{cpu}");
        }
    }

    #[test]
    fn lga1700_ddr4_and_ddr5_are_separate() {
        assert_eq!(classify_platform("GenuineIntel", "Intel Core i9-14900KF", &[26], "Z790"), Platform::Lga1700Ddr4);
        assert_eq!(classify_platform("GenuineIntel", "Intel Core i9-14900K", &[34], "Z790"), Platform::Lga1700Ddr5);
    }

    #[test]
    fn pre_lga1700_and_mobile_intel_are_unsupported() {
        for (cpu, types, board) in [
            ("Intel Core i7-10700K", 26, "Z490"),
            ("Intel Core i9-13900HX", 34, "Laptop"),
            ("Intel Core Ultra 9 185H", 34, "Laptop"),
            ("Intel Core Ultra 9 285H", 34, "Laptop"),
        ] {
            assert_eq!(classify_platform("GenuineIntel", cpu, &[types], board), Platform::Unsupported, "{cpu}");
        }
    }

    #[test]
    fn core_ultra_desktop_is_lga1851() {
        for cpu in ["Intel Core Ultra 9 285K", "Intel(R) Core(TM) Ultra 7 270K Plus", "Intel Core Ultra 5 225F"] {
            assert_eq!(classify_platform("GenuineIntel", cpu, &[34], "Z890"), Platform::Lga1851, "{cpu}");
        }
    }

    #[test]
    fn unknown_vendor_is_unsupported() {
        assert_eq!(classify_platform("", "", &[], ""), Platform::Unsupported);
    }

    #[test]
    fn detection_reads_wmi() {
        FakeWmi::new()
            .class(
                "Win32_Processor",
                vec![WmiObject::new().with("Manufacturer", "GenuineIntel").with("Name", "Intel(R) Core(TM) i9-14900KF")],
            )
            .class(
                "Win32_PhysicalMemory",
                vec![WmiObject::new().with("SMBIOSMemoryType", 0i64).with("MemoryType", 26i64)],
            )
            .class("Win32_BaseBoard", vec![WmiObject::new().with("Product", "Z790")])
            .install();
        assert_eq!(detect_current_platform(), Platform::Lga1700Ddr4);
    }

    #[test]
    fn detection_without_wmi_is_unsupported() {
        FakeWmi::new().failing("Win32_Processor").install();
        assert_eq!(detect_current_platform(), Platform::Unsupported);
    }
}
