//! Conservative DRAM IC identification from module metadata.
//!
//! Windows SMBIOS normally exposes the module part number, but not the actual
//! DRAM component marking.  This module therefore reports a die only when the
//! supplied metadata contains an explicit component/version code or matches a
//! high-confidence part-number rule.  Unknown modules stay Unknown IC instead
//! of being guessed.

use std::sync::LazyLock;

use regex::Regex;

use crate::system_identity::py_isspace;

/// `re.sub(r"\s+", "", value.upper())`. Python's `\s` is every character
/// `str.isspace()` accepts -- a no-break space, an ideographic space, the
/// 0x1C-0x1F separators -- where regex-lite's is ASCII only, so the filter is
/// written out rather than left to the regex.
fn clean(value: &str) -> String {
    value.to_uppercase().chars().filter(|c| !py_isspace(*c)).collect()
}

fn joined(values: &[&str]) -> String {
    values.join(" ").to_uppercase()
}

/// Exact module part numbers that are useful when SMBIOS exposes the per-stick
/// number rather than the retail kit SKU.
const EXACT_PART_NUMBERS: &[(&str, &str)] = &[
    // G.Skill Ripjaws V DDR4-3600 CL14 16 GB module (2x16 GB GVKA kit).
    ("F4-3600C14-16GVKA", "Samsung B-die"),
    // Acer Predator 16 GB DDR4 module confirmed by its SPD in a reference viewer on
    // the ASUS Z790-A D4 reference system.
    ("BL.9BWWR.298", "Samsung B-die"),
    // SK hynix 16 GB DDR5 OEM module used by the Z890 test system.
    // The user verified the installed HMCG78AGBUA081N modules are A-die.
    ("HMCG78AGBUA081N", "SK hynix A-die"),
    // V-Color 16 GB DDR5 module (its SPD and SMBIOS both name V-Color).
    // Its SPD stepping reads 0x41, A-die, on the Z790 MPOWER.
    ("TMXFL1680838KWK", "SK hynix A-die"),
    // G.Skill 16 GB DDR5 modules confirmed SK hynix A-die on user kits.
    ("F5-6000J2636G16G", "SK hynix A-die"),
    ("F5-6000J3040G16G", "SK hynix A-die"),
    ("F5-6000J3238G16G", "SK hynix A-die"),
    ("F5-6400J3239G16G", "SK hynix A-die"),
    ("F5-7200J3445G16G", "SK hynix A-die"),
    ("F5-7800J3646G16G", "SK hynix A-die"),
    ("F5-8000J3648G16G", "SK hynix A-die"),
    ("F5-8200J3852H16G", "SK hynix A-die"),
    // Common Crucial Ballistix 8 GB Rev. E modules.
    ("BL8G32C16U4B", "Micron E-die"),
    ("BL8G36C16U4B", "Micron E-die"),
    ("BLS8G4D30AESBK", "Micron E-die"),
    ("BLS8G4D32AESBK", "Micron E-die"),
    ("BLS16G4D30AESB", "Micron E-die"),
    ("BLS16G4D32AESB", "Micron E-die"),
];

/// Corsair label version codes.  These are only used if a version string is
/// actually present in SMBIOS metadata (for example VER4.31 or V4.31).
const CORSAIR_VERSION_CODES: &[(&str, &str)] = &[
    ("4.31", "Samsung B-die"),
    ("4.32", "Samsung C-die"),
    ("5.32", "SK hynix CJR"),
    ("5.33", "SK hynix DJR"),
    ("3.44", "Micron B-die"),
    ("8.31", "Nanya B-die"),
];

static CORSAIR_PATTERNS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    CORSAIR_VERSION_CODES
        .iter()
        .map(|(version, label)| {
            (Regex::new(&format!(r"(?:VER|VERSION|V)?\s*{}(?:\D|$)", regex::escape(version))).unwrap(), *label)
        })
        .collect()
});

static GSKILL_042: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"04213X[48S][0-9]([123459])[0-9]([A-Z])").unwrap());

/// Decode a DDR4 G.Skill 042 code when firmware happens to expose it.
///
/// Example compact form: 04213X8810B
///   first character after X = IC density
///   third character after X = IC manufacturer
///   final character = die revision
fn decode_gskill_042_code(metadata: &str) -> Option<String> {
    let compact = clean(metadata);
    let found = GSKILL_042.captures(&compact)?;
    let (manufacturer_code, revision) = (&found[1], &found[2]);
    let manufacturer = match manufacturer_code {
        "1" => "Samsung",
        "2" => "SK hynix",
        "3" => "Micron",
        "4" => "PSC",
        "5" => "Nanya",
        "9" => "JHICC",
        _ => return None,
    };
    Some(format!("{manufacturer} {revision}-die"))
}

// Explicit DRAM component markings are stronger than module-SKU guesses.
const COMPONENT_RULES: &[(&str, &str)] = &[
    ("H5CG48AGBD", "SK hynix A-die"),
    ("H5CG48MEBD", "SK hynix M-die"),
    ("H5AN8G8NCJR", "SK hynix CJR"),
    ("H5AN8G8NDJR", "SK hynix DJR"),
    ("H5AN8G8NAFR", "SK hynix AFR"),
    ("H5AN8G8NMFR", "SK hynix MFR"),
];

// Hynix OEM module part numbers often include the die family directly.
const HYNIX_TOKENS: &[(&str, &str)] = &[
    ("CJR", "SK hynix CJR"),
    ("DJR", "SK hynix DJR"),
    ("AJR", "SK hynix AJR"),
    ("MJR", "SK hynix MJR"),
    ("AFR", "SK hynix AFR"),
    ("MFR", "SK hynix MFR"),
];

static GSKILL_DDR4_BIN_A: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^F4-(?:3200|3600|3800)C14-(?:8|16)G").unwrap());
static GSKILL_DDR4_BIN_B: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^F4-(?:4000C15|4133C17|4266C17|4400C17)-(?:8|16)G").unwrap());
static BALLISTIX_REV_E: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^BL8G(?:30C15|32C16|36C16)U4B").unwrap());
static EXPLICIT_DIE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:SAMSUNG|MICRON|HYNIX|SKHYNIX)[-_ ]*([A-Z])[-_ ]?DIE").unwrap());

/// Python's `str.title()` for the ASCII text the die rule matches.
fn title(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut previous_cased = false;
    for c in text.chars() {
        if previous_cased {
            out.extend(c.to_lowercase());
        } else {
            out.extend(c.to_uppercase());
        }
        previous_cased = c.is_lowercase() || c.is_uppercase();
    }
    out
}

/// A conservative user-facing IC label.
///
/// The function deliberately prefers `Unknown IC` over a weak timing-bin
/// guess.  It can be expanded later by adding exact part numbers or explicit
/// manufacturer codes without changing the GUI.
///
/// `memory_type`, `capacity_gb` and `rank_label` are accepted from the
/// inventory, as they always were, and no rule consults them yet.
pub fn identify_dram_ic(
    part_number: &str,
    module_manufacturer: &str,
    memory_type: &str,
    capacity_gb: i64,
    rank_label: &str,
    extra_metadata: &[String],
) -> String {
    let _ = (memory_type, capacity_gb, rank_label);
    let part = clean(part_number);
    let maker = clean(module_manufacturer);
    let mut fields = vec![part_number, module_manufacturer];
    fields.extend(extra_metadata.iter().map(String::as_str));
    let metadata = joined(&fields);
    let compact_metadata = clean(&metadata);

    if let Some((_, label)) = EXACT_PART_NUMBERS.iter().find(|(known, _)| *known == part) {
        return label.to_string();
    }

    // Explicit DRAM component markings are stronger than module-SKU guesses.
    for (pattern, label) in COMPONENT_RULES {
        if compact_metadata.contains(pattern) {
            return label.to_string();
        }
    }

    // Hynix OEM module part numbers often include the die family directly.
    if maker.contains("HYNIX") || ["HMA", "HMC", "H5"].iter().any(|p| part.starts_with(p)) {
        for (token, label) in HYNIX_TOKENS {
            if compact_metadata.contains(token) {
                return label.to_string();
            }
        }
    }

    // Decode G.Skill's explicit DDR4 042 production code when available.
    if let Some(gskill_code) = decode_gskill_042_code(&metadata) {
        return gskill_code;
    }

    // Decode Corsair's explicit label version only when it is present.
    for (pattern, label) in CORSAIR_PATTERNS.iter() {
        if pattern.is_match(&metadata) {
            return label.to_string();
        }
    }

    // High-confidence G.Skill DDR4 bins.  These rules are intentionally narrow.
    if part.starts_with("F4-") || maker.contains("GSKILL") || metadata.contains("G.SKILL") {
        if GSKILL_DDR4_BIN_A.is_match(&part) {
            return "Samsung B-die".to_string();
        }
        if GSKILL_DDR4_BIN_B.is_match(&part) {
            return "Samsung B-die".to_string();
        }
    }

    // Crucial/Micron DDR4 Rev. E identifiers commonly present in part numbers.
    if part.contains("AES") {
        return "Micron E-die".to_string();
    }
    if BALLISTIX_REV_E.is_match(&part) {
        return "Micron E-die".to_string();
    }

    // Some OEM DIMMs expose an explicit die family token in the module number.
    if let Some(explicit_die) = EXPLICIT_DIE.captures(&metadata) {
        let whole = &explicit_die[0];
        let vendor = if whole.contains("HYNIX") {
            "SK hynix".to_string()
        } else {
            title(whole.split_whitespace().next().unwrap_or(""))
        };
        return format!("{vendor} {}-die", &explicit_die[1]);
    }

    // Identifying only the DRAM maker is still useful for OEM modules, but make
    // it clear that the die revision was not exposed.
    if part.starts_with("MTA") || maker.contains("MICRON") {
        return "Micron (die unknown)".to_string();
    }
    if part.starts_with("M378") || maker.contains("SAMSUNG") {
        return "Samsung (die unknown)".to_string();
    }
    if part.starts_with("HMA") || part.starts_with("HMC") || maker.contains("HYNIX") {
        return "SK hynix (die unknown)".to_string();
    }

    "Unknown IC".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identify(part_number: &str, module_manufacturer: &str) -> String {
        identify_dram_ic(part_number, module_manufacturer, "", 0, "", &[])
    }

    #[test]
    fn gskill_f5_6000_user_kit_is_hynix_a_die() {
        assert_eq!(identify("F5-6000J2636G16G", ""), "SK hynix A-die");
        assert_eq!(identify("F5-6000J2636G16G", "G.Skill"), "SK hynix A-die");
    }

    #[test]
    fn acer_predator_reference_kit_is_samsung_b_die() {
        assert_eq!(identify("BL.9BWWR.298", "Acer"), "Samsung B-die");
    }

    #[test]
    fn unknown_stays_unknown() {
        assert_eq!(identify("COMPLETELY-FAKE-PART", ""), "Unknown IC");
    }

    #[test]
    fn the_explicit_die_rule_titles_the_whole_first_word() {
        // Python's split()[0].title() on the matched text.
        assert_eq!(identify("X", "",), "Unknown IC");
        assert_eq!(identify_dram_ic("X", "", "", 0, "", &["SAMSUNG B-DIE".into()]), "Samsung B-die");
        assert_eq!(identify_dram_ic("X", "", "", 0, "", &["SAMSUNG-B-DIE".into()]), "Samsung-B-Die B-die");
    }

    #[test]
    fn unicode_whitespace_is_removed_as_pythons_backslash_s_removes_it() {
        // Python 1.1.0: identify_dram_ic("F4-3600C14-16 GVKA") is
        // "Samsung B-die" and a component marking split by an ideographic
        // space still names the die; an ASCII-only \s left both unmatched.
        assert_eq!(identify("F4-3600C14-16\u{a0}GVKA", ""), "Samsung B-die");
        assert_eq!(identify("F4-3600C14-16GVKA\x1c", ""), "Samsung B-die");
        assert_eq!(identify_dram_ic("X", "", "", 0, "", &["H5CG48\u{3000}AGBD".into()]), "SK hynix A-die");
    }
}
