//! The fonts installed on this machine, for the Settings window's font list.
//!
//! Windows records every installed font in the registry -- the machine's
//! under HKLM and the user's own under HKCU -- as "Name Style (TrueType)"
//! mapped to a file. On Linux fontconfig knows the same thing, and says so through
//! `fc-list`. A family is offered when it has a regular face; its bold face is
//! used for headings when it has one.

#[cfg(any(windows, test))]
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[cfg(windows)]
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, RegCloseKey, RegEnumValueW, RegOpenKeyExW,
};
#[cfg(windows)]
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

#[cfg(any(windows, test))]
const STYLE_WORDS: &[&str] = &[
    "Regular", "Bold", "Italic", "Oblique", "Light", "Semilight", "SemiLight", "Semibold", "SemiBold", "Medium",
    "Black", "Thin", "ExtraLight", "Extra Light", "ExtraBold", "Extra Bold", "Heavy", "Condensed", "Narrow",
];

/// "Consolas Bold (TrueType)" -> ("Consolas", "Bold").
#[cfg(any(windows, test))]
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

#[cfg(any(windows, test))]
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
#[cfg(any(windows, test))]
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

#[cfg(windows)]
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

#[cfg(windows)]
fn system_fonts_dir() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts")
}

/// Every family installed for this machine or this user.
#[cfg(not(windows))]
pub fn installed_families() -> Vec<Family> {
    let listing = std::process::Command::new("fc-list")
        .args(["--format", "%{family[0]}\t%{style[0]}\t%{spacing}\t%{file}\n"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).into_owned())
        .unwrap_or_default();
    families_from_fc_list(&listing)
}

/// Group `fc-list --format '%{family[0]}\t%{style[0]}\t%{spacing}\t%{file}\n'` into
/// families. Pure, so it can be tested. A file can answer to several names; the
/// first is the one to show.
#[cfg(any(not(windows), test))]
pub fn families_from_fc_list(listing: &str) -> Vec<Family> {
    let mut found: std::collections::BTreeMap<String, (Option<PathBuf>, Option<PathBuf>, bool)> = Default::default();
    for line in listing.lines() {
        let mut fields = line.split('\t');
        let (Some(family), Some(style), Some(spacing), Some(file)) =
            (fields.next(), fields.next(), fields.next(), fields.next())
        else {
            continue;
        };
        let path = PathBuf::from(file);
        if family.is_empty() || !is_font_file(&path) {
            continue;
        }
        let entry = found.entry(family.to_string()).or_default();
        // fontconfig's spacing: 90 is dual width, 100 mono, 110 charcell.
        // Proportional prints nothing.
        entry.2 |= spacing.parse::<i32>().is_ok_and(|spacing| spacing >= 90);
        if style.eq_ignore_ascii_case("Regular") || style.eq_ignore_ascii_case("Book") {
            entry.0.get_or_insert(path);
        } else if style.eq_ignore_ascii_case("Bold") {
            entry.1.get_or_insert(path);
        }
    }
    let mut families: Vec<Family> = found
        .into_iter()
        .filter_map(|(name, (regular, bold, fixed))| {
            let monospace = fixed || MONOSPACE.iter().any(|known| known.eq_ignore_ascii_case(&name));
            Some(Family { regular: regular?, bold, monospace, name })
        })
        .collect();
    families.sort_by(|a, b| b.monospace.cmp(&a.monospace).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    families
}

/// The file fontconfig gives for a pattern, if it is one a window can load.
#[cfg(not(windows))]
pub fn matched_face(pattern: &str) -> Option<PathBuf> {
    let out = std::process::Command::new("fc-match").args(["--format", "%{file}", pattern]).output().ok()?;
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    (out.status.success() && is_font_file(&path) && path.is_file()).then_some(path)
}

/// Every family installed for this machine or this user.
#[cfg(windows)]
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
        // A file outside the fonts folder keeps its own path, however this
        // platform spells an absolute one.
        let absolute = if cfg!(windows) { "C:\\Users\\x\\my.otf" } else { "/home/x/my.otf" };
        let entries = vec![
            ("Arial (TrueType)".to_string(), "arial.ttf".to_string()),
            ("Consolas (TrueType)".to_string(), "consola.ttf".to_string()),
            ("Consolas Bold (TrueType)".to_string(), "consolab.ttf".to_string()),
            ("Consolas Italic (TrueType)".to_string(), "consolai.ttf".to_string()),
            ("Some Bitmap Font".to_string(), "font.fon".to_string()),
            ("My Font (TrueType)".to_string(), absolute.to_string()),
        ];
        let families = families_from_entries(&entries, dir);
        let names: Vec<&str> = families.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["Consolas", "Arial", "My Font"]);
        assert_eq!(families[0].bold.as_deref(), Some(dir.join("consolab.ttf").as_path()));
        assert_eq!(families[2].regular, PathBuf::from(absolute));
    }

    #[test]
    fn fc_list_lines_become_families() {
        let listing = "JetBrains Mono\tRegular\t100\t/fonts/jb.ttf\n\
                       JetBrains Mono\tBold\t100\t/fonts/jbb.ttf\n\
                       JetBrains Mono\tItalic\t100\t/fonts/jbi.ttf\n\
                       Noto Sans\tRegular\t\t/fonts/noto.otf\n\
                       Icons\tSolid\t\t/fonts/icons.woff2\n\
                       No Regular Face\tBold\t\t/fonts/bold.ttf\n\
                       truncated line\n";
        let families = families_from_fc_list(listing);
        let names: Vec<&str> = families.iter().map(|family| family.name.as_str()).collect();
        assert_eq!(names, vec!["JetBrains Mono", "Noto Sans"], "fixed width first, and only what can be drawn");
        assert!(families[0].monospace);
        assert_eq!(families[0].regular, PathBuf::from("/fonts/jb.ttf"));
        assert_eq!(families[0].bold.as_deref(), Some(Path::new("/fonts/jbb.ttf")));
        assert!(!families[1].monospace);
        assert_eq!(families[1].bold, None);
    }

    #[cfg(windows)]
    #[test]
    fn this_machine_has_consolas() {
        // Every supported Windows ships it.
        assert!(installed_families().iter().any(|f| f.name == "Consolas" && f.monospace));
    }
}
