#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Jot - A modern text editor replacement for Notepad
//!
//! Built with Rust using GPUI, Jot provides an enjoyable text writing experience
//! while occupying the space between simple text editors and rich text processors.

mod actions;
mod app;
mod assets;
mod autocomplete;
mod components;
mod spell;
mod spell_editor;
mod state;
mod theme;

use app::JotApp;
use assets::Assets;
use gpui_kit::component::{Theme, ThemeRegistry, TitleBar};
use gpui_kit::*;
use state::Settings;

fn main() {
    env_logger::init();

    let app = gpui_kit::application().with_assets(Assets);

    app.run(move |cx| {
        gpui_kit::init(cx);
        // jot is one window: closing it quits, on macOS too.
        cx.set_quit_mode(QuitMode::LastWindowClosed);
        actions::init(cx);
        load_fonts(cx);
        let settings = Settings::load();
        load_themes(cx, &settings.theme);

        let mut window_size = size(px(1000.0), px(700.0));
        if let Some(display) = cx.primary_display() {
            let display_size = display.bounds().size;
            window_size.width = window_size.width.min(display_size.width * 0.85);
            window_size.height = window_size.height.min(display_size.height * 0.85);
        }

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::centered(window_size, cx)),
            titlebar: Some(TitleBar::title_bar_options()),
            window_min_size: Some(size(px(400.), px(300.))),
            kind: WindowKind::Normal,
            ..Default::default()
        };

        gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| JotApp::new(window, cx))
        })
        .expect("Failed to open window");
    });
}

fn load_fonts(cx: &mut App) {
    if let Ok(Some(work_sans)) = Assets.load("fonts/WorkSans-Regular.ttf") {
        if let Err(e) = cx.text_system().add_fonts(vec![work_sans]) {
            log::error!("Failed to load Work Sans font: {}", e);
        }
    }

    if let Ok(Some(jetbrains)) = Assets.load("fonts/JetBrainsMono-Regular.ttf") {
        if let Err(e) = cx.text_system().add_fonts(vec![jetbrains]) {
            log::error!("Failed to load JetBrains Mono font: {}", e);
        }
    }
}

fn load_themes(cx: &mut App, initial_theme: &str) {
    let initial_theme = initial_theme.to_string();

    if let Some(theme_dir) = theme::find_themes_dir() {
        if let Err(e) = ThemeRegistry::watch_dir(theme_dir, cx, move |cx| {
            let registry = ThemeRegistry::global(cx);
            let theme_name = if registry.themes().contains_key(initial_theme.as_str()) {
                initial_theme.as_str()
            } else {
                "Alduin"
            };

            if let Some(theme_config) = registry.themes().get(theme_name).cloned() {
                Theme::global_mut(cx).apply_config(&theme_config);
            }
        }) {
            log::error!("Failed to load themes: {}", e);
        }
    }
}
