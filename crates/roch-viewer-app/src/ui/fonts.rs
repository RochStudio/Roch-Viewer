//! The fonts installed on this machine, for the Settings window's font list.
//!
//! Windows records every installed font in the registry -- the machine's
//! under HKLM and the user's own under HKCU -- as "Name Style (TrueType)"
//! mapped to a file. A family is offered when it has a regular face; its bold
//! face is used for headings when it has one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
};
use windows::core::w;

/// One family's faces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Family {
    pub name: String,
    pub regular: PathBuf,
    pub bold: Option<PathBuf>,
    pub monospace: bool,
}

/// Families known to be fixed-pitch, listed first: the tables are laid out
/// for one, and a proportional face still works but looks less even.
const MONOSPACE: &[&str] = &[
    "Consolas",
    "Cascadia Mono",
    "Cascadia Code",
    "Courier New",
    "Lucida Console",
    "Lucida Sans Typewriter",
    "JetBrains Mono",
    "Fira Code",
    "Fira Mono",
    "Source Code Pro",
    "Hack",
    "DejaVu Sans Mono",
    "Roboto Mono",
    "Ubuntu Mono",
    "Inconsolata",
    "IBM Plex Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "SimSun-ExtB",
    "MS Gothic",
];

const STYLE_WORDS: &[&str] = &[
    "Regular", "Bold", "Italic", "Oblique", "Light", "Semilight", "SemiLight", "Semibold", "SemiBold", "Medium",
    "Black", "Thin", "ExtraLight", "Extra Light", "ExtraBold", "Extra Bold", "Heavy", "Condensed", "Narrow",
];

/// "Consolas Bold (TrueType)" -> ("Consolas", "Bold").
fn split_name(entry: &str) -> (String, String) {
    let base = entry.split(" (").next().unwrap_or(entry).trim();
    let mut family: Vec<&str> = base.split(' ').collect();
    let mut style: Vec<&str> = Vec::new();
    while let Some(last) = family.last() {
        if family.len() > 1 && STYLE_WORDS.iter().any(|w| w.eq_ignore_ascii_case(last)) {
            style.insert(0, family.pop().unwrap());
        } else {
            break;
        }
    }
    (family.join(" "), style.join(" "))
}

fn font_path(value: &str, fonts_dir: &Path) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() { path } else { fonts_dir.join(path) }
}

fn is_font_file(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase).as_deref(),
        Some("ttf") | Some("otf") | Some("ttc")
    )
}

/// Group registry entries into families. Pure, so it can be tested.
pub fn families_from_entries(entries: &[(String, String)], fonts_dir: &Path) -> Vec<Family> {
    let mut regular: BTreeMap<String, PathBuf> = BTreeMap::new();
    let mut bold: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, value) in entries {
        // Collections ("Cambria & Cambria Math") name several families in
        // one file; the first is the one the file opens as.
        let name = name.split(" & ").next().unwrap_or(name);
        let path = font_path(value, fonts_dir);
        if !is_font_file(&path) {
            continue;
        }
        let (family, style) = split_name(name);
        if family.is_empty() {
            continue;
        }
        match style.as_str() {
            "" | "Regular" => {
                regular.entry(family).or_insert(path);
            }
            "Bold" => {
                bold.entry(family).or_insert(path);
            }
            _ => {}
        }
    }
    let mut families: Vec<Family> = regular
        .into_iter()
        .map(|(name, path)| Family {
            monospace: MONOSPACE.iter().any(|m| m.eq_ignore_ascii_case(&name)),
            bold: bold.get(&name).cloned(),
            regular: path,
            name,
        })
        .collect();
    families.sort_by(|a, b| b.monospace.cmp(&a.monospace).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    families
}

fn read_values(root: HKEY) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut key = HKEY::default();
    // SAFETY: opening a key for reading into a handle we close below.
    let opened = unsafe {
        RegOpenKeyExW(root, w!("SOFTWARE\\Microsoft\\Windows NT\\CurrentVersion\\Fonts"), Some(0), KEY_READ, &mut key)
    };
    if opened.is_err() {
        return out;
    }
    for index in 0.. {
        let mut name = [0u16; 512];
        let mut name_len = name.len() as u32;
        let mut data = [0u8; 2048];
        let mut data_len = data.len() as u32;
        let mut kind = 0u32;
        // SAFETY: every buffer and length is a local of the size given.
        let status = unsafe {
            RegEnumValueW(
                key,
                index,
                Some(windows::core::PWSTR(name.as_mut_ptr())),
                &mut name_len,
                None,
                Some(&mut kind as *mut u32),
                Some(data.as_mut_ptr()),
                Some(&mut data_len),
            )
        };
        if status.is_err() {
            break;
        }
        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let words: Vec<u16> = data[..data_len as usize].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let value = String::from_utf16_lossy(&words).trim_end_matches('\0').to_string();
        out.push((name, value));
    }
    // SAFETY: the handle opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    out
}

fn system_fonts_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts")
}

/// Every family installed for this machine or this user.
pub fn installed_families() -> Vec<Family> {
    let mut entries = read_values(HKEY_LOCAL_MACHINE);
    entries.extend(read_values(HKEY_CURRENT_USER));
    families_from_entries(&entries, &system_fonts_dir())
}

/// One family by name, or `None` when it is not installed.
pub fn find_family(name: &str) -> Option<Family> {
    installed_families().into_iter().find(|f| f.name.eq_ignore_ascii_case(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_into_family_and_style() {
        assert_eq!(split_name("Consolas Bold (TrueType)"), ("Consolas".into(), "Bold".into()));
        assert_eq!(split_name("Cascadia Code SemiBold (TrueType)"), ("Cascadia Code".into(), "SemiBold".into()));
        assert_eq!(split_name("Arial (TrueType)"), ("Arial".into(), String::new()));
        assert_eq!(split_name("Black (TrueType)"), ("Black".into(), String::new()));
    }

    #[test]
    fn families_pair_regular_and_bold_and_put_monospace_first() {
        let dir = Path::new("C:\\Windows\\Fonts");
        let entries = vec![
            ("Arial (TrueType)".to_string(), "arial.ttf".to_string()),
            ("Consolas (TrueType)".to_string(), "consola.ttf".to_string()),
            ("Consolas Bold (TrueType)".to_string(), "consolab.ttf".to_string()),
            ("Consolas Italic (TrueType)".to_string(), "consolai.ttf".to_string()),
            ("Some Bitmap Font".to_string(), "font.fon".to_string()),
            ("My Font (TrueType)".to_string(), "C:\\Users\\x\\my.otf".to_string()),
        ];
        let families = families_from_entries(&entries, dir);
        let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Consolas", "Arial", "My Font"]);
        assert_eq!(families[0].bold.as_deref(), Some(dir.join("consolab.ttf").as_path()));
        assert_eq!(families[2].regular, PathBuf::from("C:\\Users\\x\\my.otf"));
    }

    #[test]
    fn this_machine_has_consolas() {
        // Every supported Windows ships it.
        assert!(installed_families().iter().any(|f| f.name == "Consolas" && f.monospace));
    }
}
