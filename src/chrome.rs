//! The main window's options and the one close check.

use crate::app::JotApp;
use crate::components::attempt_close_window;
use gpui_kit::component::{Root, TitleBar};
use gpui_kit::*;

/// The main window: a transparent title bar that jot draws itself, with the
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
    let app = Root::read(window, cx).view().clone().downcast::<JotApp>();
    match app {
        Ok(app) => {
            let app_state = app.read(cx).app_state().clone();
            attempt_close_window(app_state, window, cx)
        }
        Err(_) => true,
    }
}

/// Closes `window` if `can_close` agrees. `window.remove_window()` on its own
/// skips the check.
pub fn close_window(window: &mut Window, cx: &mut App) {
    if can_close(window, cx) {
        window.remove_window();
    }
}

/// Quits once every window agrees to close. A window that asks about unsaved
/// changes closes itself afterwards, and jot quits with its last window.
pub fn quit(cx: &mut App) {
    // Deferred, so the window that sent Quit is free to answer too.
    cx.defer(|cx| {
        let all_agree = cx.windows().into_iter().all(|window| {
            window
                .update(cx, |_, window, cx| can_close(window, cx))
                .unwrap_or(true)
        });
        if all_agree {
            cx.quit();
        }
    });
}
