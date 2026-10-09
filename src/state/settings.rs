use gpui_kit::SharedString;
use gpui_kit::component::input::Keymap;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const ZOOM_LEVELS: &[u32] = &[50, 75, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// jot's settings, saved as JSON in `<config>/jot/settings.json`.
///
/// A field missing from the file takes its default, so files written before
/// the field existed still load.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: SharedString,
    pub font_family: SharedString,
    pub font_size: u32,
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub xml_auto_complete: bool,
    pub autocomplete: bool,
    pub spell_check: bool,
    pub tab_size: u32,
    pub restore_session: bool,
    pub zoom_level: u32,
    /// The editor's keybinding scheme: `cua`, `emacs` or `vim`.
    pub keymap: Keymap,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Alduin".into(),
            font_family: "JetBrains Mono".into(),
            font_size: 14,
            word_wrap: false,
            line_numbers: true,
            xml_auto_complete: false,
            autocomplete: true,
            spell_check: true,
            tab_size: 4,
            restore_session: false,
            zoom_level: 100,
            keymap: Keymap::Cua,
        }
    }
}

impl Settings {
    fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("jot").join("settings.json"))
    }

    pub fn load() -> Self {
        Self::config_path()
            .and_then(|path| std::fs::read_to_string(&path).ok())
            .and_then(|content| serde_json::from_str(&content).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = Self::config_path() else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(content) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(&path, content);
        }
    }

    pub fn zoom_in(&mut self) {
        if let Some(pos) = ZOOM_LEVELS.iter().position(|&z| z == self.zoom_level)
            && pos + 1 < ZOOM_LEVELS.len()
        {
            self.zoom_level = ZOOM_LEVELS[pos + 1];
        }
    }

    pub fn zoom_out(&mut self) {
        if let Some(pos) = ZOOM_LEVELS.iter().position(|&z| z == self.zoom_level)
            && pos > 0
        {
            self.zoom_level = ZOOM_LEVELS[pos - 1];
        }
    }

    pub fn reset_zoom(&mut self) {
        self.zoom_level = 100;
    }

    pub fn effective_font_size(&self) -> f32 {
        self.font_size as f32 * (self.zoom_level as f32 / 100.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_without_newer_fields_load_with_their_defaults() {
        let old = r#"{
            "theme": "Gruvbox Dark",
            "font_family": "JetBrains Mono",
            "font_size": 16,
            "word_wrap": true,
            "line_numbers": true,
            "xml_auto_complete": false,
            "autocomplete": true,
            "tab_size": 4,
            "restore_session": false,
            "zoom_level": 100
        }"#;
        let settings: Settings = serde_json::from_str(old).unwrap();
        assert_eq!(settings.theme, "Gruvbox Dark");
        assert_eq!(settings.font_size, 16);
        assert!(settings.spell_check);
        assert_eq!(settings.keymap, Keymap::Cua);
    }

    #[test]
    fn keymap_round_trips() {
        let settings = Settings {
            keymap: Keymap::Vim,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""keymap":"vim""#), "{json}");
        let loaded: Settings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.keymap, Keymap::Vim);
    }
}
