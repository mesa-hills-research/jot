use crate::fonts::DEFAULT_EDITOR_FONT;
use gpui_kit::component::font_picker::FontSettings;
use gpui_kit::component::input::Keymap;
use gpui_kit::writing::SuggestionMode;
use gpui_kit::{SharedString, px};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

pub const ZOOM_LEVELS: &[u32] = &[50, 75, 90, 100, 110, 125, 150, 175, 200, 250, 300];

/// jot's settings, saved as JSON in `<config>/jot/settings.json`.
///
/// A field missing from the file takes its default, so files written before
/// the field existed still load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The chosen color theme's name. Without one, the theme is light or
    /// dark as the system is.
    pub theme: Option<SharedString>,
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
    pub autocomplete: SuggestionMode,
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
            theme: None,
            editor_font: FontSettings::new(DEFAULT_EDITOR_FONT)
                .with_size(px(14.))
                .with_line_height(1.5),
            word_wrap: false,
            line_numbers: true,
            xml_auto_complete: false,
            autocomplete: SuggestionMode::Quiet,
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
    /// Where jot keeps its settings.
    pub fn config_path() -> Option<PathBuf> {
        dirs::config_dir().map(|p| p.join("jot").join("settings.json"))
    }

    /// The settings saved at `path`. A missing or empty file gives the
    /// defaults. So does a file that isn't settings, after what it holds is
    /// copied to `settings.damaged.json` or, when that holds something else,
    /// `settings.damaged-2.json` and so on. The file stays until settings
    /// are saved over it. The error is a file that couldn't be read or
    /// copied, which mustn't be saved over.
    pub fn load(path: &Path) -> io::Result<Self> {
        let content = match std::fs::read(path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(error) => return Err(error),
        };
        if content.is_empty() {
            return Ok(Self::default());
        }
        if let Some(settings) = std::str::from_utf8(&content).ok().and_then(Self::from_json) {
            return Ok(settings);
        }
        let kept = keep_copy(path, &content)?;
        log::warn!(
            "jot couldn\u{2019}t read its settings, so it starts with the defaults. They\u{2019}re kept in {}.",
            kept.display()
        );
        Ok(Self::default())
    }

    /// Reads settings saved as JSON, by this version or an earlier one. A
    /// value it can't read, such as one edited by hand, keeps its default,
    /// and the other settings load. `None` means the text isn't a JSON
    /// object.
    fn from_json(content: &str) -> Option<Self> {
        let value: serde_json::Value = serde_json::from_str(content).ok()?;
        let fields = value.as_object()?;
        let mut settings: Settings = serde_json::from_value(value.clone()).unwrap_or_else(|_| {
            let mut readable = serde_json::Map::new();
            for (key, field) in fields {
                readable.insert(key.clone(), field.clone());
                if serde_json::from_value::<Settings>(readable.clone().into()).is_err() {
                    log::warn!(
                        "jot couldn\u{2019}t read the setting {key}, so it has its default."
                    );
                    readable.remove(key);
                }
            }
            serde_json::from_value(readable.into()).unwrap_or_default()
        });
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

    /// The settings as jot saves them.
    pub fn to_json(&self) -> io::Result<String> {
        Ok(serde_json::to_string_pretty(self)?)
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

/// Keeps `content`, from the damaged settings file at `path`, in the first
/// of `settings.damaged.json`, `settings.damaged-2.json` and so on that
/// holds it already or is free, and returns that file. A name is taken only
/// by making a new file, so no copy, this jot's or another's, is ever
/// written over.
fn keep_copy(path: &Path, content: &[u8]) -> io::Result<PathBuf> {
    use std::io::Write as _;
    for n in 1..=1000 {
        let kept = match n {
            1 => path.with_extension("damaged.json"),
            n => path.with_extension(format!("damaged-{n}.json")),
        };
        let mut copy = match std::fs::File::create_new(&kept) {
            Ok(copy) => copy,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if std::fs::read(&kept).is_ok_and(|kept| kept == content) {
                    return Ok(kept);
                }
                continue;
            }
            Err(error) => return Err(error),
        };
        if let Err(error) = copy.write_all(content).and_then(|()| copy.sync_all()) {
            drop(copy);
            // Made by this load, under a name no other file had.
            let _ = std::fs::remove_file(&kept);
            return Err(error);
        }
        return Ok(kept);
    }
    Err(io::Error::other(
        "jot has kept a thousand damaged settings files already",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for a test, removed afterwards.
    struct Folder(PathBuf);

    impl Folder {
        fn new(name: &str) -> Self {
            let folder = std::env::temp_dir()
                .join(format!("jot-settings-test-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&folder);
            Self(folder)
        }

        fn file(&self) -> PathBuf {
            self.0.join("jot").join("settings.json")
        }
    }

    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Settings with every field changed from its default.
    fn changed_settings() -> Settings {
        Settings {
            theme: Some("Gruvbox Dark".into()),
            editor_font: FontSettings::new("Inconsolata")
                .with_size(px(16.))
                .with_line_height(1.7),
            word_wrap: true,
            line_numbers: false,
            xml_auto_complete: true,
            autocomplete: SuggestionMode::Eager,
            spell_check: false,
            tab_size: 2,
            restore_session: true,
            zoom_level: 150,
            keymap: Keymap::Vim,
            smooth_caret: true,
        }
    }

    #[test]
    fn every_setting_survives_a_save_and_a_fresh_load() {
        let folder = Folder::new("restart");
        let save = |settings: &Settings| {
            std::fs::create_dir_all(folder.file().parent().unwrap()).unwrap();
            let json = settings.to_json().unwrap();
            crate::file_write::write_whole(
                &folder.file(),
                json.as_bytes(),
                &folder.0.join("backups"),
            )
            .unwrap();
        };
        let settings = changed_settings();
        save(&settings);
        assert_eq!(Settings::load(&folder.file()).unwrap(), settings);

        // Saving again replaces the file, and leaves nothing beside it.
        let settings = Settings::default();
        save(&settings);
        assert_eq!(Settings::load(&folder.file()).unwrap(), settings);
        let files: Vec<_> = std::fs::read_dir(folder.0.join("jot")).unwrap().collect();
        assert_eq!(files.len(), 1);
    }

    #[test]
    fn a_missing_file_gives_the_defaults() {
        let folder = Folder::new("missing");
        assert_eq!(Settings::load(&folder.file()).unwrap(), Settings::default());
    }

    #[test]
    fn every_damaged_file_is_kept_and_the_defaults_load() {
        let folder = Folder::new("damaged");
        let file = folder.file();
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let copy = |n: usize| {
            let name = match n {
                1 => "settings.damaged.json".to_string(),
                n => format!("settings.damaged-{n}.json"),
            };
            std::fs::read(file.with_file_name(name)).ok()
        };
        // A copy another jot kept earlier stays as it is.
        std::fs::write(file.with_file_name("settings.damaged.json"), "earlier").unwrap();
        let damaged = [&b"{\"theme\": \"Gruv"[..], b"[1, 2]", b"\xff\xfe"];
        for (n, text) in damaged.iter().enumerate() {
            std::fs::write(&file, text).unwrap();
            assert_eq!(Settings::load(&file).unwrap(), Settings::default());
            // The file stays until settings are saved over it, and its text
            // is kept in a copy of its own.
            assert_eq!(std::fs::read(&file).unwrap(), *text);
            assert_eq!(copy(n + 2).as_deref(), Some(*text));
        }
        assert_eq!(copy(1).as_deref(), Some(&b"earlier"[..]));

        // Loading it again keeps no second copy.
        assert_eq!(Settings::load(&file).unwrap(), Settings::default());
        assert_eq!(copy(damaged.len() + 2), None);

        // An empty file holds nothing to keep.
        std::fs::write(&file, "").unwrap();
        assert_eq!(Settings::load(&file).unwrap(), Settings::default());
        let names = std::fs::read_dir(file.parent().unwrap()).unwrap().count();
        assert_eq!(names, damaged.len() + 2);
    }

    #[test]
    fn a_value_that_cant_be_read_keeps_its_default_alone() {
        let settings = Settings::from_json(
            r#"{
                "theme": "Ayu Dark",
                "word_wrap": "yes",
                "zoom_level": 150,
                "keymap": "dvorak",
                "smooth_caret": true
            }"#,
        )
        .unwrap();
        assert_eq!(
            settings,
            Settings {
                theme: Some("Ayu Dark".into()),
                zoom_level: 150,
                smooth_caret: true,
                ..Settings::default()
            }
        );
    }

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
        assert_eq!(settings.theme.as_deref(), Some("Gruvbox Dark"));
        assert!(settings.spell_check);
        assert_eq!(settings.autocomplete, SuggestionMode::Quiet);
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
        assert_eq!(load("true"), SuggestionMode::Quiet);
        assert_eq!(load("false"), SuggestionMode::Off);
        assert_eq!(load(r#""off""#), SuggestionMode::Off);
        assert_eq!(load(r#""quiet""#), SuggestionMode::Quiet);
        assert_eq!(load(r#""eager""#), SuggestionMode::Eager);
        assert_eq!(
            Settings::from_json("{}").unwrap().autocomplete,
            SuggestionMode::Quiet
        );
    }

    #[test]
    fn autocomplete_mode_round_trips() {
        for mode in [
            SuggestionMode::Off,
            SuggestionMode::Quiet,
            SuggestionMode::Eager,
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
