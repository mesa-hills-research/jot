use crate::autocomplete::AutocompleteMode;
use crate::fonts::DEFAULT_EDITOR_FONT;
use gpui_kit::component::font_picker::FontSettings;
use gpui_kit::component::input::Keymap;
use gpui_kit::{SharedString, px};
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
    /// The editor font: family, weight, style, size, line height and
    /// OpenType features. Files written before it existed name the family
    /// and size alone, in `font_family` and `font_size`.
    pub editor_font: FontSettings,
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub xml_auto_complete: bool,
    /// How eagerly word suggestions show: `off`, `quiet` or `eager`. Files
    /// written when this was a switch hold `true` for Quiet or `false` for
    /// Off.
    pub autocomplete: AutocompleteMode,
    pub spell_check: bool,
    pub tab_size: u32,
    pub restore_session: bool,
    pub zoom_level: u32,
    /// The editor's keybinding scheme: `cua`, `emacs` or `vim`.
    pub keymap: Keymap,
    /// Whether the caret glides to where it moves instead of jumping.
    pub smooth_caret: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Alduin".into(),
            editor_font: FontSettings::new(DEFAULT_EDITOR_FONT)
                .with_size(px(14.))
                .with_line_height(1.5),
            word_wrap: false,
            line_numbers: true,
            xml_auto_complete: false,
            autocomplete: AutocompleteMode::Quiet,
            spell_check: true,
            tab_size: 4,
            restore_session: false,
            zoom_level: 100,
            keymap: Keymap::Cua,
            smooth_caret: false,
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
            .and_then(|content| Self::from_json(&content))
            .unwrap_or_default()
    }

    /// Reads settings saved as JSON, by this version or an earlier one.
    fn from_json(content: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(content).ok()?;
        let mut settings: Settings = serde_json::from_value(value.clone()).ok()?;
        if value.get("editor_font").is_none() {
            let mut font = settings.editor_font.clone();
            if let Some(family) = value.get("font_family").and_then(|family| family.as_str()) {
                font = font.with_family(family.to_string());
            }
            if let Some(size) = value.get("font_size").and_then(|size| size.as_f64()) {
                font = font.with_size(px(size as f32));
            }
            settings.editor_font = font;
        }
        Some(settings)
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

    /// The editor font's size at the current zoom, in pixels.
    pub fn effective_font_size(&self) -> f32 {
        f32::from(self.editor_font.size()) * (self.zoom_level as f32 / 100.0)
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
        let settings = Settings::from_json(old).unwrap();
        assert_eq!(settings.theme, "Gruvbox Dark");
        assert!(settings.spell_check);
        assert_eq!(settings.autocomplete, AutocompleteMode::Quiet);
        assert_eq!(settings.keymap, Keymap::Cua);
        assert!(!settings.smooth_caret);
        // The family and size become the editor font.
        assert_eq!(settings.editor_font.family(), "JetBrains Mono");
        assert_eq!(settings.editor_font.size(), px(16.));
        assert_eq!(settings.editor_font.line_height(), 1.5);
    }

    #[test]
    fn the_editor_font_round_trips() {
        let settings = Settings {
            editor_font: FontSettings::new("Inconsolata")
                .with_size(px(15.))
                .with_weight(gpui_kit::FontWeight::BOLD)
                .with_line_height(1.7)
                .with_feature("calt", false),
            ..Settings::default()
        };
        let json = serde_json::to_string_pretty(&settings).unwrap();
        let loaded = Settings::from_json(&json).unwrap();
        assert_eq!(loaded.editor_font, settings.editor_font);
        assert_eq!(loaded.effective_font_size(), 15.);
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

    #[test]
    fn the_old_suggestions_switch_loads_as_a_mode() {
        let load = |autocomplete: &str| {
            let json = format!(r#"{{"theme": "Alduin", "autocomplete": {autocomplete}}}"#);
            Settings::from_json(&json).unwrap().autocomplete
        };
        assert_eq!(load("true"), AutocompleteMode::Quiet);
        assert_eq!(load("false"), AutocompleteMode::Off);
        assert_eq!(load(r#""off""#), AutocompleteMode::Off);
        assert_eq!(load(r#""quiet""#), AutocompleteMode::Quiet);
        assert_eq!(load(r#""eager""#), AutocompleteMode::Eager);
        assert_eq!(
            Settings::from_json("{}").unwrap().autocomplete,
            AutocompleteMode::Quiet
        );
    }

    #[test]
    fn autocomplete_mode_round_trips() {
        for mode in [
            AutocompleteMode::Off,
            AutocompleteMode::Quiet,
            AutocompleteMode::Eager,
        ] {
            let settings = Settings {
                autocomplete: mode,
                ..Settings::default()
            };
            let json = serde_json::to_string_pretty(&settings).unwrap();
            assert_eq!(Settings::from_json(&json).unwrap().autocomplete, mode);
        }
        let json = serde_json::to_string(&Settings::default()).unwrap();
        assert!(json.contains(r#""autocomplete":"quiet""#), "{json}");
    }

    #[test]
    fn smooth_caret_round_trips() {
        let settings = Settings {
            smooth_caret: true,
            ..Settings::default()
        };
        let json = serde_json::to_string(&settings).unwrap();
        assert!(json.contains(r#""smooth_caret":true"#), "{json}");
        assert!(Settings::from_json(&json).unwrap().smooth_caret);
    }
}
