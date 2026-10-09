//! jot's windows: opening them, and what happens when jot is launched while
//! it is running.

use crate::app::JotApp;
use crate::chrome;
use crate::instance::Incoming;
use crate::state::AppState;
use gpui_kit::component::{Root, WindowExt, notification::Notification};
use gpui_kit::*;
use std::path::PathBuf;

#[cfg(test)]
mod tests;

/// What launching jot again without files does while it runs.
#[derive(Clone, Copy, Debug, PartialEq)]
#[allow(dead_code)]
pub enum WithoutFiles {
    /// Opens another window, as VS Code does.
    NewWindow,
    /// Brings the most recently used window to the front.
    FocusWindow,
}

/// The choice jot makes. Files always open in the most recently used
/// window, which comes to the front.
pub const LAUNCH_WITHOUT_FILES: WithoutFiles = WithoutFiles::NewWindow;

/// A request for jot to open files, or a window.
pub enum Launch {
    /// A later launch of jot, with the files on its command line.
    Instance(Incoming),
    /// Files the system asked jot to open, such as from the macOS Finder.
    Files(Vec<PathBuf>),
}

/// Carries out a request from a later launch or from the system.
pub fn handle(launch: Launch, cx: &mut App) {
    let paths = match launch {
        Launch::Instance(incoming) => match incoming.accept() {
            Some(paths) => paths,
            // The launch stopped waiting and runs on its own.
            None => return,
        },
        Launch::Files(paths) => paths,
    };
    log::info!("A launch asks for {} files", paths.len());
    if !paths.is_empty() {
        open_paths(paths, cx);
        return;
    }
    let window = match (LAUNCH_WITHOUT_FILES, most_recent_window(cx)) {
        (WithoutFiles::FocusWindow, Some(window)) => Some(window),
        _ => open_window(Vec::new(), cx),
    };
    if let Some(window) = window {
        bring_to_front(window, cx);
    }
}

/// Opens a window with `paths` in its tabs, or a new document without any.
///
/// From an action, defer this: the window that sent it is busy, and the new
/// one is placed by it.
pub fn open_window(paths: Vec<PathBuf>, cx: &mut App) -> Option<AnyWindowHandle> {
    let mut options = chrome::window_options(cx);
    // Each new window sits a little below and to the right of the one in
    // use, so it doesn't hide it exactly.
    if let Some(bounds) = most_recent_window(cx)
        .and_then(|window| window.update(cx, |_, window, _| window.bounds()).ok())
    {
        let offset = point(px(28.), px(28.));
        let cascaded = Bounds::new(bounds.origin + offset, bounds.size);
        let fits = cx.displays().iter().any(|display| {
            let screen = display.bounds();
            screen.contains(&cascaded.origin) && screen.contains(&cascaded.bottom_right())
        });
        if fits {
            options.window_bounds = Some(WindowBounds::Windowed(cascaded));
        }
    }
    let app_state = AppState::global(cx);
    match gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| JotApp::new(app_state, paths, window, cx))
    }) {
        Ok((window, _)) => Some(window),
        Err(error) => {
            log::error!("jot couldn\u{2019}t open a window: {error:#}");
            None
        }
    }
}

/// Opens `paths`. A file that is open already comes to the front in its
/// window, and the rest open in the most recently used window, or a new one.
pub fn open_paths(paths: Vec<PathBuf>, cx: &mut App) {
    let mut to_open = Vec::new();
    let mut shown = None;
    for path in paths {
        match find_tab(&path, cx) {
            Some((handle, index)) => {
                handle
                    .update(cx, |_, window, cx| {
                        if let Some(app) = jot_app(window, cx) {
                            app.update(cx, |app, cx| app.show_tab(index, window, cx));
                        }
                    })
                    .ok();
                shown = Some(handle);
            }
            None => to_open.push(path),
        }
    }
    if !to_open.is_empty() {
        match most_recent_window(cx) {
            Some(handle) => {
                handle
                    .update(cx, |_, window, cx| {
                        if let Some(app) = jot_app(window, cx) {
                            app.update(cx, |app, cx| app.open_paths(to_open, window, cx));
                        }
                    })
                    .ok();
                shown = Some(handle);
            }
            None => shown = open_window(to_open, cx),
        }
    }
    if let Some(window) = shown {
        bring_to_front(window, cx);
    }
}

/// The open jot windows, the most recently used first.
pub fn recent_windows(cx: &mut App) -> Vec<AnyWindowHandle> {
    AppState::global(cx).update(cx, |state, cx| state.recent_windows(cx))
}

pub fn most_recent_window(cx: &mut App) -> Option<AnyWindowHandle> {
    recent_windows(cx).into_iter().next()
}

/// The window and tab that hold the file at `path`.
fn find_tab(path: &std::path::Path, cx: &mut App) -> Option<(AnyWindowHandle, usize)> {
    recent_windows(cx).into_iter().find_map(|handle| {
        let index = handle
            .update(cx, |_, window, cx| {
                let app = jot_app(window, cx)?;
                let state = app.read(cx).window_state().clone();
                state.read(cx).tab_for(path, cx)
            })
            .ok()??;
        Some((handle, index))
    })
}

/// Brings `handle`'s window to the front, with the focus in its document.
fn bring_to_front(handle: AnyWindowHandle, cx: &mut App) {
    handle
        .update(cx, |_, window, cx| {
            if !window.is_window_active() {
                window.activate_window();
            }
            if let Some(app) = jot_app(window, cx) {
                app.update(cx, |app, cx| app.focus_editor(window, cx));
            }
        })
        .ok();
    if cfg!(target_os = "macos") {
        cx.activate(true);
    }
}

/// The jot view in `window`.
pub fn jot_app(window: &mut Window, cx: &mut App) -> Option<Entity<JotApp>> {
    Root::read(window, cx)
        .view()
        .clone()
        .downcast::<JotApp>()
        .ok()
}

/// Shows why some files couldn't be opened.
pub fn report_open_errors(errors: Vec<String>, window: &mut Window, cx: &mut App) {
    for error in errors {
        window.push_notification(Notification::error(error), cx);
    }
}
