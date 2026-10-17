//! Rail definitions and voltage formatting for the Voltages view.
//!
//! Deliberately free of any hardware access so the row builders, the Summary
//! layout and the tests can use it without the driver.

pub const SOURCE_PM_TABLE: &str = "pmtable"; // SMU PM table, amd::smu_voltages
pub const SOURCE_PMIC: &str = "pmic"; // DDR5 PMIC over SMBus, memory::ddr5_pmic
pub const SOURCE_SUPERIO: &str = "superio"; // board Super I/O over LPC, sensors::superio_lpc

/// One displayable rail plus the range that makes a reading believable.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VoltageRail {
    pub key: &'static str,
    pub label: &'static str,
    pub group: &'static str,
    pub min_volts: f64,
    pub max_volts: f64,
    pub source: &'static str,
}

const fn rail(key: &'static str, label: &'static str, group: &'static str, min_volts: f64, max_volts: f64, source: &'static str) -> VoltageRail {
    VoltageRail { key, label, group, min_volts, max_volts, source }
}

pub const RAILS: &[VoltageRail] = &[
    rail("vddcr_vdd", "VDDCR_VDD", "Core", 0.40, 1.55, SOURCE_PM_TABLE),
    rail("vddcr_soc", "VDDCR_SOC", "SoC", 0.70, 1.40, SOURCE_PM_TABLE),
    rail("cldo_vddq", "VDDP", "Fabric", 0.60, 1.35, SOURCE_PM_TABLE),
    rail("vddg_ccd", "VDDG CCD", "Fabric", 0.60, 1.35, SOURCE_PM_TABLE),
    rail("vddg_iod", "VDDG IOD", "Fabric", 0.60, 1.35, SOURCE_PM_TABLE),
    // VDDIO and VTT are board sensor rails: proven absent from both the PM
    // table and the DDR5 PMIC, so they come from the Super I/O.
    rail("vddio_mem", "VDDIO", "Memory", 0.90, 1.80, SOURCE_SUPERIO),
    rail("vtt", "VTT", "Memory", 1.50, 2.10, SOURCE_SUPERIO),
    // DRAM VDD/VDDQ/VPP live on the DIMM's own PMIC; the SMU PM table does
    // not carry them at all -- see amd::smu_voltages::CONFIRMED_VOLTAGE_OFFSETS.
    rail("dram_vdd", "DRAM VDD", "Memory", 0.90, 1.65, SOURCE_PMIC),
    rail("dram_vddq", "DRAM VDDQ", "Memory", 0.90, 1.65, SOURCE_PMIC),
    // VPP is the 1.8 V nominal wordline rail, hence the higher band.
    rail("dram_vpp", "DRAM VPP", "Memory", 1.50, 2.10, SOURCE_PMIC),
    // Named as HWiNFO does, which reads the same SVI3 telemetry: its
    // "CPU VDD_MISC Voltage (SVI3 TFN)" is this rail, to the millivolt.
    rail("cdd_misc", "VDD_MISC", "Misc", 0.40, 1.35, SOURCE_PM_TABLE),
];

pub fn rail_by_key(key: &str) -> Option<&'static VoltageRail> {
    RAILS.iter().find(|rail| rail.key == key)
}

pub const SUMMARY_HIDDEN_RAILS: &[&str] = &["vtt"];

pub const PER_MODULE_RAILS: &[&str] = &["dram_vdd", "dram_vddq", "dram_vpp"];

pub const CANDIDATE_MIN_VOLTS: f64 = 0.40;
pub const CANDIDATE_MAX_VOLTS: f64 = 2.10;

/// The rail voltage, or why it is not believable.
pub fn validate_voltage(rail: &VoltageRail, volts: f64) -> Result<f64, String> {
    if !volts.is_finite() {
        return Err(format!("{} is not finite", rail.label));
    }
    if !(rail.min_volts <= volts && volts <= rail.max_volts) {
        return Err(format!(
            "{} {:.4} V is outside {:.2}-{:.2} V",
            rail.label, volts, rail.min_volts, rail.max_volts
        ));
    }
    Ok(volts)
}

/// Render a rail voltage the way memory tuners read it (millivolt step).
pub fn format_volts(volts: f64) -> String {
    format!("{volts:.3} V")
}

/// True when a decoded float could plausibly be any rail on this platform.
pub fn is_candidate_voltage(volts: f64) -> bool {
    volts.is_finite() && (CANDIDATE_MIN_VOLTS..=CANDIDATE_MAX_VOLTS).contains(&volts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_unique_and_looked_up() {
        for (i, a) in RAILS.iter().enumerate() {
            assert!(RAILS[i + 1..].iter().all(|b| b.key != a.key));
        }
        assert_eq!(rail_by_key("vtt").unwrap().source, SOURCE_SUPERIO);
        assert!(rail_by_key("nope").is_none());
    }

    #[test]
    fn validation_bounds_and_message() {
        let vdd = rail_by_key("vddcr_vdd").unwrap();
        assert_eq!(validate_voltage(vdd, 1.2), Ok(1.2));
        assert_eq!(
            validate_voltage(vdd, 2.0).unwrap_err(),
            "VDDCR_VDD 2.0000 V is outside 0.40-1.55 V"
        );
        assert!(validate_voltage(vdd, f64::NAN).is_err());
    }

    #[test]
    fn formatting_and_candidates() {
        assert_eq!(format_volts(1.1), "1.100 V");
        assert!(is_candidate_voltage(1.35));
        assert!(!is_candidate_voltage(0.2));
        assert!(!is_candidate_voltage(f64::INFINITY));
    }
}
