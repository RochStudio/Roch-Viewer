//! Per-user settings, in the same file and keys the Python build used, so an
//! upgrade keeps the chosen appearance.

use std::path::PathBuf;

use serde_json::{Map, Value};

/// What a first run opens in, and what an unreadable setting falls back to.
#[cfg(test)]
pub const DEFAULT_APPEARANCE_MODE: &str = "Light";

#[derive(Clone, Debug, Default)]
pub struct Settings {
    values: Map<String, Value>,
}

impl Settings {
    /// `%LOCALAPPDATA%\RochViewer\settings.json`, or beside the home folder.
    pub fn path() -> PathBuf {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
            .unwrap_or_default();
        base.join("RochViewer").join("settings.json")
    }

    /// The saved settings, or none when there are none or they do not parse.
    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let values = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|value| match value {
                Value::Object(map) => Some(map),
                _ => None,
            })
            .unwrap_or_default();
        Settings { values }
    }

    /// Every saved key.
    pub fn map(&self) -> &Map<String, Value> {
        &self.values
    }

    #[cfg(test)]
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn get_bool(&self, key: &str) -> bool {
        self.values.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// Merge one setting into the file, leaving the others alone.
    pub fn save(&mut self, key: &str, value: Value) {
        self.save_to(&Self::path(), key, value);
    }

    pub fn save_to(&mut self, path: &std::path::Path, key: &str, value: Value) {
        // Re-read first: another window of this program may have saved since.
        let mut current = Self::load_from(path);
        current.values.insert(key.to_string(), value);
        self.values = current.values.clone();
        let result = path
            .parent()
            .map(std::fs::create_dir_all)
            .unwrap_or(Ok(()))
            .and_then(|_| std::fs::write(path, Value::Object(current.values).to_string()));
        if let Err(error) = result {
            eprintln!("Could not save setting {key}: {error}");
        }
    }

    /// "Light" or "Dark".
    #[cfg(test)]
    pub fn appearance_mode(&self) -> &'static str {
        match self.get("appearance_mode").and_then(Value::as_str).map(str::to_lowercase).as_deref() {
            Some("dark") => "Dark",
            Some("light") => "Light",
            _ => DEFAULT_APPEARANCE_MODE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("rochviewer-settings-test-{}-{name}", std::process::id()))
            .join("settings.json")
    }

    #[test]
    fn light_is_the_default_and_bad_values_fall_back() {
        assert_eq!(Settings::default().appearance_mode(), "Light");
        let path = temp_path("bad");
        let mut settings = Settings::load_from(&path);
        settings.save_to(&path, "appearance_mode", Value::from("purple"));
        assert_eq!(Settings::load_from(&path).appearance_mode(), "Light");
        settings.save_to(&path, "appearance_mode", Value::from("DARK"));
        assert_eq!(Settings::load_from(&path).appearance_mode(), "Dark");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn saving_one_key_keeps_the_others() {
        let path = temp_path("merge");
        let mut settings = Settings::load_from(&path);
        settings.save_to(&path, "telemetry_auto_open", Value::from(true));
        settings.save_to(&path, "appearance_mode", Value::from("Dark"));
        let reloaded = Settings::load_from(&path);
        assert!(reloaded.get_bool("telemetry_auto_open"));
        assert_eq!(reloaded.appearance_mode(), "Dark");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn unreadable_file_is_empty() {
        let path = temp_path("garbage");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "[1, 2").unwrap();
        assert!(Settings::load_from(&path).get("appearance_mode").is_none());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
