//! jot's color themes, built into the binary, and the one in force: the
//! theme the settings name or, without one, a light or a dark theme as the
//! system is.

use gpui_kit::component::{Theme, ThemeConfig, ThemeRegistry, ThemeSet};
use gpui_kit::{App, Global, SharedString, WindowAppearance};
use rust_embed::RustEmbed;
use std::rc::Rc;

/// The theme files: GPUI Kit's set, and Matrix.
#[derive(RustEmbed)]
#[folder = "themes"]
struct ThemeFiles;

/// The theme shown when the settings name none and the system is light.
pub const DEFAULT_LIGHT_THEME: &str = "macOS Classic Light";
/// The theme shown when the settings name none and the system is dark.
pub const DEFAULT_DARK_THEME: &str = "macOS Classic Dark";

/// Whether the system is dark, as the windows last reported it.
struct SystemDark(bool);

impl Global for SystemDark {}

/// Registers jot's themes and shows `chosen`, or the system's.
pub fn init(chosen: Option<&str>, cx: &mut App) {
    ThemeRegistry::global_mut(cx).add_bundled_themes(bundled());
    cx.set_global(SystemDark(is_dark(cx.window_appearance())));
    apply(chosen, cx);
}

/// Every theme in jot's theme files.
fn bundled() -> Vec<ThemeConfig> {
    ThemeFiles::iter()
        .filter_map(|file| {
            let data = ThemeFiles::get(&file)?.data;
            serde_json::from_slice::<ThemeSet>(&data)
                .inspect_err(|error| log::error!("Couldn\u{2019}t read the theme {file}: {error}"))
                .ok()
        })
        .flat_map(|set| set.themes)
        .collect()
}

/// The themes the settings page offers, sorted without case. GPUI Kit's own
/// two stay out, since they only stand in for a theme that's missing.
pub fn names(cx: &App) -> Vec<SharedString> {
    let registry = ThemeRegistry::global(cx);
    let mut names: Vec<SharedString> = registry
        .themes()
        .values()
        .filter(|theme| !is_kit_default(theme, registry))
        .map(|theme| theme.name.clone())
        .collect();
    names.sort_by_key(|name| name.to_lowercase());
    names
}

/// The name of the offered theme called `name`, as the settings page lists
/// it, or `None` when jot has no such theme.
pub fn offered(name: &str, cx: &App) -> Option<SharedString> {
    find(name, cx).map(|theme| theme.name.clone())
}

fn find(name: &str, cx: &App) -> Option<Rc<ThemeConfig>> {
    let registry = ThemeRegistry::global(cx);
    registry
        .theme(name)
        .filter(|theme| !is_kit_default(theme, registry))
        .cloned()
}

fn is_kit_default(theme: &Rc<ThemeConfig>, registry: &ThemeRegistry) -> bool {
    registry
        .default_themes()
        .values()
        .any(|default| Rc::ptr_eq(default, theme))
}

/// Shows the theme named `chosen`. Without one, or when jot has no theme of
/// that name, it shows the default light or dark theme as the system is.
pub fn apply(chosen: Option<&str>, cx: &mut App) {
    let dark = cx.try_global::<SystemDark>().is_some_and(|system| system.0);
    let fallback = if dark {
        DEFAULT_DARK_THEME
    } else {
        DEFAULT_LIGHT_THEME
    };
    let theme = chosen
        .and_then(|name| find(name, cx))
        .or_else(|| find(fallback, cx))
        .unwrap_or_else(|| {
            let registry = ThemeRegistry::global(cx);
            if dark {
                registry.default_dark_theme().clone()
            } else {
                registry.default_light_theme().clone()
            }
        });
    // `update` also rebuilds the Base layer's copy of the theme, which draws
    // the scrollbars.
    Theme::update(cx, |current| current.apply_config(&theme));
}

/// Notes the system's appearance, as a window reports it, and switches
/// between the light and dark themes when it changes and no theme is chosen.
pub fn set_system_appearance(appearance: WindowAppearance, chosen: Option<&str>, cx: &mut App) {
    let dark = is_dark(appearance);
    if cx
        .try_global::<SystemDark>()
        .is_some_and(|system| system.0 == dark)
    {
        return;
    }
    cx.set_global(SystemDark(dark));
    apply(chosen, cx);
}

fn is_dark(appearance: WindowAppearance) -> bool {
    matches!(
        appearance,
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;

    fn shown(cx: &App) -> SharedString {
        Theme::global(cx).theme_name().clone()
    }

    #[test]
    fn every_theme_file_loads() {
        for file in ThemeFiles::iter() {
            let data = ThemeFiles::get(&file).unwrap().data;
            let set: ThemeSet = serde_json::from_slice(&data).unwrap_or_else(|error| {
                panic!("{file}: {error}");
            });
            assert!(!set.themes.is_empty(), "{file} has no themes");
        }
        let names: Vec<_> = bundled().into_iter().map(|theme| theme.name).collect();
        for name in [
            DEFAULT_LIGHT_THEME,
            DEFAULT_DARK_THEME,
            "Windows Light",
            "Alduin",
            "Matrix",
        ] {
            assert!(names.iter().any(|theme| theme == name), "{name} is missing");
        }
    }

    #[gpui_kit::test]
    fn the_chosen_theme_shows_and_without_one_the_system_decides(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(Some("Gruvbox Dark"), cx);
            assert_eq!(shown(cx), "Gruvbox Dark");
            // A chosen theme stays when the system changes.
            set_system_appearance(WindowAppearance::Light, Some("Gruvbox Dark"), cx);
            set_system_appearance(WindowAppearance::Dark, Some("Gruvbox Dark"), cx);
            assert_eq!(shown(cx), "Gruvbox Dark");

            apply(None, cx);
            assert_eq!(shown(cx), DEFAULT_DARK_THEME);
            set_system_appearance(WindowAppearance::Light, None, cx);
            assert_eq!(shown(cx), DEFAULT_LIGHT_THEME);

            // A theme jot doesn't offer, such as GPUI Kit's own default from
            // an earlier jot, follows the system too.
            apply(Some("Default Dark"), cx);
            assert_eq!(shown(cx), DEFAULT_LIGHT_THEME);
            assert_eq!(offered("Default Dark", cx), None);
        });
    }

    #[gpui_kit::test]
    fn the_settings_page_offers_every_theme_but_the_kits_own(cx: &mut TestAppContext) {
        cx.update(|cx| {
            gpui_kit::init(cx);
            init(None, cx);
            let names = names(cx);
            assert_eq!(names.len(), bundled().len());
            assert!(names.iter().any(|name| name == "macOS Classic Light"));
            assert!(names.iter().any(|name| name == "Windows Dark"));
            assert!(!names.iter().any(|name| name.starts_with("Default ")));
            assert_eq!(offered("gruvbox dark", cx).as_deref(), Some("Gruvbox Dark"));
        });
    }
}
