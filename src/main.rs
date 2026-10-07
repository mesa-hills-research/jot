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
mod spell_actions;
mod state;
mod theme;

use app::JotApp;
use assets::Assets;
use gpui::{
    px, size, App, AppContext, Application, AssetSource, Bounds, WindowBounds, WindowKind,
    WindowOptions,
};
use gpui_component::{Root, TitleBar};
use state::Settings;

fn main() {
    env_logger::init();

    let app = Application::new().with_assets(Assets);

    app.run(move |cx| {
        gpui_component::init(cx);
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

        let window_bounds = Bounds::centered(None, window_size, cx);

        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(window_bounds)),
            titlebar: Some(TitleBar::title_bar_options()),
            window_min_size: Some(gpui::Size {
                width: px(400.),
                height: px(300.),
            }),
            kind: WindowKind::Normal,
            ..Default::default()
        };

        cx.spawn(async move |cx| {
            let window = cx
                .open_window(options, |window, cx| {
                    let jot_app = cx.new(|cx| JotApp::new(window, cx));
                    cx.new(|cx| Root::new(jot_app, window, cx))
                })
                .expect("Failed to open window");

            let _ = window.update(cx, |_, _window, cx| {
                cx.on_release(|_, cx| {
                    cx.quit();
                })
                .detach();
            });

            Ok::<_, anyhow::Error>(())
        })
        .detach();
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
        if let Err(e) = gpui_component::ThemeRegistry::watch_dir(theme_dir, cx, move |cx| {
            let registry = gpui_component::ThemeRegistry::global(cx);
            let theme_name = if registry.themes().contains_key(initial_theme.as_str()) {
                initial_theme.as_str()
            } else {
                "Alduin"
            };

            if let Some(theme_config) = registry.themes().get(theme_name).cloned() {
                gpui_component::Theme::global_mut(cx).apply_config(&theme_config);
            }
        }) {
            log::error!("Failed to load themes: {}", e);
        }
    }
}