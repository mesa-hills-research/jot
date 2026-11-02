//! The windows' options, the one close check, and quitting.

use crate::components::attempt_close_window;
use crate::launch::jot_app;
use crate::state::AppState;
use gpui_kit::component::TitleBar;
use gpui_kit::*;

/// A jot window: a transparent title bar that jot draws itself, with the
/// macOS traffic lights at (9, 9) and the drag left to the app, as GPUI Kit's
/// `TitleBar` expects.
pub fn window_options(cx: &App) -> WindowOptions {
    let mut window_size = size(px(1000.0), px(700.0));
    if let Some(display) = cx.primary_display() {
        let display_size = display.bounds().size;
        window_size.width = window_size.width.min(display_size.width * 0.85);
        window_size.height = window_size.height.min(display_size.height * 0.85);
    }

    WindowOptions {
        titlebar: Some(TitlebarOptions {
            // Hidden in the window, shown by the taskbar and window switchers.
            title: Some("Jot".into()),
            ..TitleBar::title_bar_options()
        }),
        window_bounds: Some(WindowBounds::centered(window_size, cx)),
        // 500 px or less keeps Windows 11 snap layouts working.
        window_min_size: Some(size(px(400.), px(300.))),
        // Linux: jot draws its own frame on every compositor, as on macOS and
        // Windows. Left unset, KDE, Sway and X11 add a system title bar above
        // jot's own.
        window_decorations: Some(WindowDecorations::Client),
        kind: WindowKind::Normal,
        ..TitleBar::window_options()
    }
}

/// The one close check. The macOS and Windows close buttons, Alt+F4, the
/// Linux compositor and title bar, Close Window and Quit all end here.
///
/// With unsaved changes it asks about each document, returns `false`, and
/// removes the window once the user has answered.
pub fn can_close(window: &mut Window, cx: &mut App) -> bool {
    match jot_app(window, cx) {
        Some(app) => {
            let window_state = app.read(cx).window_state().clone();
            attempt_close_window(window_state, window, cx)
        }
        None => true,
    }
}

/// Closes `window` if `can_close` agrees. `window.remove_window()` on its own
/// skips the check.
pub fn close_window(window: &mut Window, cx: &mut App) {
    if can_close(window, cx) {
        window.remove_window();
    }
}

/// Quits once every window agrees to close. A window with unsaved changes
/// comes to the front and asks about them, and one with saves running waits
/// for them. Cancel in a question stops quitting.
pub fn quit(cx: &mut App) {
    AppState::global(cx).update(cx, |state, _| state.quitting = true);
    // Deferred, so the window that sent Quit is free to answer too.
    cx.defer(quit_step);
}

/// A step of quitting, taken when Quit is chosen and again after each wait
/// and answer, until it's cancelled. It looks at every window and save as
/// they are now, and quits only when, in this same step, no window has
/// changes to ask about or saves to wait for, and the settings are written.
pub fn quit_step(cx: &mut App) {
    let app_state = AppState::global(cx);
    if !app_state.read(cx).quitting {
        return;
    }
    for handle in cx.windows() {
        // A window that can't close yet asks or waits, and once it has
        // closed takes the next step.
        let agrees = handle
            .update(cx, |_, window, cx| {
                let agrees = can_close(window, cx);
                if !agrees {
                    window.activate_window();
                }
                agrees
            })
            .unwrap_or(true);
        if !agrees {
            return;
        }
    }
    if app_state.read(cx).is_saving_settings() {
        let settings_saved = app_state.read(cx).settings_saved();
        cx.spawn(async move |cx| {
            settings_saved.await;
            cx.update(quit_step);
        })
        .detach();
        return;
    }
    for handle in cx.windows() {
        handle
            .update(cx, |_, window, _| window.remove_window())
            .ok();
    }
    cx.quit();
}
