//! Fonts: the ones jot bundles, and the font files the user adds, which jot
//! keeps in `<config>/jot/fonts` and registers at start-up.

use anyhow::{Context as _, anyhow};
use gpui_kit::assets::fonts;
use gpui_kit::component::font_picker::FontCatalog;
use gpui_kit::component::{FontOverrides, Theme};
use gpui_kit::{App, SharedString};
use std::borrow::Cow;
use std::path::{Path, PathBuf};

/// The editor font when the settings name none, or name one that is missing.
pub const DEFAULT_EDITOR_FONT: &str = fonts::JETBRAINS_MONO_FAMILY;

/// The interface font.
pub const UI_FONT: &str = fonts::WORK_SANS_FAMILY;

/// The interface's monospace font, which GPUI Kit draws keyboard shortcuts in.
pub const MONO_FONT: &str = fonts::JETBRAINS_MONO_FAMILY;

/// Font files jot reads from the fonts folder.
const FONT_EXTENSIONS: [&str; 4] = ["ttf", "otf", "ttc", "otc"];

/// The fonts jot bundles: Work Sans for the interface and JetBrains Mono for
/// the editor, upright and italic, as GPUI Kit's variable fonts, so every
/// weight draws as designed.
pub fn bundled_fonts() -> Vec<Cow<'static, [u8]>> {
    fonts::all()
}

/// Puts the interface and monospace fonts over every theme: applying a theme
/// would otherwise set them to the theme's own, the system's fonts for jot's
/// themes. One update, and the kit's dialogs, menus and tooltips, drawn
/// outside jot's views, use Work Sans too.
pub fn apply_app_fonts(cx: &mut App) {
    Theme::update(cx, |theme| theme.set_font_overrides(app_font_overrides()));
}

fn app_font_overrides() -> FontOverrides {
    FontOverrides::new()
        .with_font_family(UI_FONT)
        .with_mono_font_family(MONO_FONT)
}

/// The folder for the font files the user added.
pub fn user_fonts_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|dir| dir.join("jot").join("fonts"))
}

fn is_font_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| FONT_EXTENSIONS.contains(&extension.to_lowercase().as_str()))
}

/// The font files the user added earlier.
pub fn user_fonts() -> Vec<Cow<'static, [u8]>> {
    let Some(entries) = user_fonts_dir().and_then(|dir| std::fs::read_dir(dir).ok()) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(|entry| Some(entry.ok()?.path()))
        .filter(|path| is_font_file(path))
        .collect();
    paths.sort();
    paths
        .into_iter()
        .filter_map(|path| match std::fs::read(&path) {
            Ok(data) => Some(Cow::Owned(data)),
            Err(error) => {
                log::warn!("Couldn't read the font {}: {error}", path.display());
                None
            }
        })
        .collect()
}

/// Registers the bundled fonts and the ones the user added.
pub fn register_fonts(cx: &mut App) {
    for font in bundled_fonts().into_iter().chain(user_fonts()) {
        if let Err(error) = cx.text_system().add_fonts(vec![font]) {
            log::error!("Couldn't load a font: {error}");
        }
    }
}

/// Whether the text system can draw `family`.
pub fn is_available(family: &str, cx: &App) -> bool {
    cx.text_system()
        .all_font_names()
        .iter()
        .any(|name| name.eq_ignore_ascii_case(family))
}

/// A font file the user chose, copied into the fonts folder.
pub struct ImportedFont {
    pub data: Vec<u8>,
    /// The families in the file.
    pub families: Vec<SharedString>,
    /// Whether the fonts folder already had this file, so it is registered
    /// already.
    pub already_imported: bool,
}

/// Copies the font file at `path` into the fonts folder, keeping its name,
/// unless the folder has the same file already.
pub fn import_font_file(path: &Path) -> anyhow::Result<ImportedFont> {
    let data =
        std::fs::read(path).with_context(|| format!("Couldn\u{2019}t read {}", path.display()))?;
    let families: Vec<SharedString> = FontCatalog::from_fonts(&[&data])
        .families()
        .iter()
        .map(|family| family.name().clone())
        .collect();
    if families.is_empty() {
        return Err(anyhow!(
            "{} isn\u{2019}t a font file jot can read.",
            file_name(path)
        ));
    }

    let dir = user_fonts_dir().ok_or_else(|| anyhow!("jot has no settings folder."))?;
    std::fs::create_dir_all(&dir)?;
    let mut target = dir.join(file_name(path));
    let mut copy = 1;
    while target.exists() {
        if std::fs::read(&target).is_ok_and(|existing| existing == data) {
            return Ok(ImportedFont {
                data,
                families,
                already_imported: true,
            });
        }
        copy += 1;
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("font");
        let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("ttf");
        target = dir.join(format!("{stem} ({copy}).{extension}"));
    }
    std::fs::write(&target, &data)
        .with_context(|| format!("Couldn\u{2019}t copy the font to {}", target.display()))?;
    Ok(ImportedFont {
        data,
        families,
        already_imported: false,
    })
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "font".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::component::ThemeConfig;
    use gpui_kit::{TestAppContext, px};
    use std::rc::Rc;

    /// Switching themes keeps jot's fonts, though no theme of jot's names
    /// a font, and the interface font reaches the theme the kit's overlays
    /// draw with.
    #[gpui_kit::test]
    fn a_theme_switch_keeps_the_app_fonts(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            apply_app_fonts(cx);
            let theme = |json| Rc::new(serde_json::from_value::<ThemeConfig>(json).unwrap());
            for config in [
                theme(serde_json::json!({ "name": "Dark", "mode": "dark" })),
                theme(serde_json::json!({ "name": "Light", "mode": "light", "font.size": 15 })),
            ] {
                Theme::update(cx, |theme| theme.apply_config(&config));
                let theme = Theme::global(cx);
                assert_eq!(theme.font_family.as_ref(), UI_FONT);
                assert_eq!(theme.mono_font_family.as_ref(), MONO_FONT);
            }
            // Sizes stay the theme's: jot overrides only the families.
            assert_eq!(Theme::global(cx).font_size, px(15.));
        });
    }

    #[test]
    fn jot_bundles_both_families_upright_and_italic() {
        assert_eq!(bundled_fonts().len(), 4);
        assert_eq!(UI_FONT, "Work Sans");
        assert_eq!(MONO_FONT, "JetBrains Mono");
    }
}
