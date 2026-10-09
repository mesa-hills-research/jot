#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

//! Jot - A modern text editor replacement for Notepad
//!
//! Built with Rust using GPUI, Jot provides an enjoyable text writing experience
//! while occupying the space between simple text editors and rich text processors.

mod actions;
mod app;
mod args;
mod assets;
mod autocomplete;
mod chrome;
mod components;
mod fonts;
mod instance;
mod launch;
mod spell;
mod state;
mod theme;

use args::Args;
use assets::Assets;
use gpui_kit::component::{Theme, ThemeRegistry};
use gpui_kit::*;
use instance::Startup;
use launch::Launch;
use state::AppState;

fn main() {
    env_logger::init();

    let cwd = std::env::current_dir().unwrap_or_default();
    let args = Args::parse(std::env::args_os().skip(1), &cwd);

    // A jot that is running already opens the files, and this one is done.
    let server = if args.new_instance {
        None
    } else {
        match instance::start(&args.paths) {
            Startup::Primary(server) => Some(server),
            Startup::HandedOver => return,
            Startup::Alone => None,
        }
    };

    // Later launches, and files the system asks jot to open, reach the
    // windows through this channel.
    let (launches, received) = async_channel::unbounded::<Launch>();
    if let Some(server) = server {
        let launches = launches.clone();
        server.serve(move |incoming| {
            launches.try_send(Launch::Instance(incoming)).ok();
        });
    }

    let app = gpui_kit::application().with_assets(Assets);
    // macOS sends files opened from the Finder, or dropped on the Dock icon,
    // to the running jot.
    app.on_open_urls(move |urls| {
        let paths: Vec<_> = urls
            .iter()
            .filter_map(|url| args::file_url_to_path(url))
            .collect();
        if !paths.is_empty() {
            launches.try_send(Launch::Files(paths)).ok();
        }
    });
    // Clicking the Dock icon with every window closed opens one.
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            launch::open_window(Vec::new(), cx);
        }
    });

    app.run(move |cx| {
        gpui_kit::init(cx);
        // Windows and Linux quit with the last window. macOS keeps jot
        // running, as Mac apps do, until Quit.
        cx.set_quit_mode(QuitMode::Default);
        fonts::register_fonts(cx);
        fonts::keep_mono_font(cx);
        let app_state = AppState::init(cx);
        let settings = app_state.read(cx).settings.clone();
        actions::init(&settings, cx);
        load_themes(cx, &settings.theme);

        if launch::open_window(args.paths, cx).is_none() {
            panic!("jot couldn\u{2019}t open a window");
        }

        cx.spawn(async move |cx| {
            while let Ok(launch) = received.recv().await {
                cx.update(|cx| launch::handle(launch, cx));
            }
        })
        .detach();
    });
}

fn load_themes(cx: &mut App, initial_theme: &str) {
    let initial_theme = initial_theme.to_string();

    if let Some(theme_dir) = theme::find_themes_dir()
        && let Err(e) = ThemeRegistry::watch_dir(theme_dir, cx, move |cx| {
            let registry = ThemeRegistry::global(cx);
            let theme_name = if registry.themes().contains_key(initial_theme.as_str()) {
                initial_theme.as_str()
            } else {
                "Alduin"
            };

            if let Some(theme_config) = registry.themes().get(theme_name).cloned() {
                Theme::update(cx, |theme| theme.apply_config(&theme_config));
            }
        })
    {
        log::error!("Failed to load themes: {}", e);
    }
}
