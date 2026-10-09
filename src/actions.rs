//! Global actions, keybindings and the menus for Jot.
//!
//! macOS shows the menus in the system menu bar. Windows and Linux show them
//! in the title bar, through GPUI Kit's `AppMenuBar`.

use crate::chrome;
use crate::state::Settings;
use gpui_kit::component::{GlobalState, input};
use gpui_kit::*;

actions!(
    jot,
    [
        NewTab,
        NewWindow,
        NewFromTemplate,
        OpenFile,
        Save,
        SaveAs,
        CloseTab,
        CloseWindow,
        GoToLine,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        ToggleWordWrap,
        ToggleLineNumbers,
        OpenSettings,
        NextTab,
        PreviousTab,
        Quit,
        Hide,
        HideOthers,
        ShowAll,
        Minimize,
        Zoom,
    ]
);

/// Context identifier for the application.
pub const APP_CONTEXT: &str = "Jot";

/// Registers the app-wide actions and the keybindings, and sets the menus.
/// Undo, Redo, Cut, Copy, Paste, Select All, Find and Replace are the
/// editor's own, with its own bindings.
pub fn init(settings: &Settings, cx: &mut App) {
    cx.on_action(|_: &Quit, cx| chrome::quit(cx));
    cx.on_action(|_: &Hide, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &ShowAll, cx| cx.unhide_other_apps());

    #[cfg(target_os = "macos")]
    let bindings = vec![
        KeyBinding::new("cmd-t", NewTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-n", NewWindow, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-t", NewFromTemplate, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-o", OpenFile, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-s", Save, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-s", SaveAs, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-w", CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-h", Hide, None),
        KeyBinding::new("alt-cmd-h", HideOthers, None),
        KeyBinding::new("cmd-m", Minimize, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-g", GoToLine, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-=", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-+", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("cmd--", ZoomOut, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-0", ResetZoom, Some(APP_CONTEXT)),
        KeyBinding::new("alt-z", ToggleWordWrap, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-l", ToggleLineNumbers, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-,", OpenSettings, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(APP_CONTEXT)),
    ];

    // Alt+F4 closes the window through the system, which runs the close check.
    #[cfg(not(target_os = "macos"))]
    let bindings = vec![
        KeyBinding::new("ctrl-t", NewTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-n", NewWindow, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-t", NewFromTemplate, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-o", OpenFile, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-s", Save, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-s", SaveAs, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-w", CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, None),
        KeyBinding::new("ctrl-g", GoToLine, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-=", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-+", ZoomIn, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl--", ZoomOut, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-0", ResetZoom, Some(APP_CONTEXT)),
        KeyBinding::new("alt-z", ToggleWordWrap, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-l", ToggleLineNumbers, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-,", OpenSettings, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-tab", NextTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, Some(APP_CONTEXT)),
    ];

    cx.bind_keys(bindings);
    set_menus(settings, cx);
}

/// Sets the menus, which show the settings' Word Wrap and Line Numbers.
/// A title bar's `AppMenuBar` picks the change up on `reload`.
pub fn set_menus(settings: &Settings, cx: &mut App) {
    cx.set_menus(menus(settings));
    GlobalState::global_mut(cx)
        .set_app_menus(menus(settings).into_iter().map(Menu::owned).collect());
}

fn menus(settings: &Settings) -> Vec<Menu> {
    let file = Menu::new("File").items([
        MenuItem::action("New Tab", NewTab),
        MenuItem::action("New Window", NewWindow),
        MenuItem::action("New from Template...", NewFromTemplate),
        MenuItem::action("Open...", OpenFile),
        MenuItem::separator(),
        MenuItem::action("Save", Save),
        MenuItem::action("Save As...", SaveAs),
        MenuItem::separator(),
        MenuItem::action("Close Tab", CloseTab),
        MenuItem::action("Close Window", CloseWindow),
    ]);
    let edit = Menu::new("Edit").items([
        MenuItem::os_action("Undo", input::Undo, OsAction::Undo),
        MenuItem::os_action("Redo", input::Redo, OsAction::Redo),
        MenuItem::separator(),
        MenuItem::os_action("Cut", input::Cut, OsAction::Cut),
        MenuItem::os_action("Copy", input::Copy, OsAction::Copy),
        MenuItem::os_action("Paste", input::Paste, OsAction::Paste),
        MenuItem::separator(),
        MenuItem::os_action("Select All", input::SelectAll, OsAction::SelectAll),
        MenuItem::separator(),
        MenuItem::action("Find...", input::Search),
        MenuItem::action("Replace...", input::Replace),
        MenuItem::action("Go to Line...", GoToLine),
    ]);
    let view = Menu::new("View").items([
        MenuItem::action("Zoom In", ZoomIn),
        MenuItem::action("Zoom Out", ZoomOut),
        MenuItem::action("Reset Zoom", ResetZoom),
        MenuItem::separator(),
        MenuItem::action("Word Wrap", ToggleWordWrap).checked(settings.word_wrap),
        MenuItem::action("Line Numbers", ToggleLineNumbers).checked(settings.line_numbers),
        MenuItem::separator(),
        MenuItem::action("Settings", OpenSettings),
    ]);

    if cfg!(target_os = "macos") {
        vec![
            // macOS titles the first menu with the app's name.
            Menu::new("Jot").items([
                MenuItem::action("About Jot", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("Settings...", OpenSettings),
                MenuItem::separator(),
                MenuItem::os_submenu("Services", SystemMenuType::Services),
                MenuItem::separator(),
                MenuItem::action("Hide Jot", Hide),
                MenuItem::action("Hide Others", HideOthers),
                MenuItem::action("Show All", ShowAll),
                MenuItem::separator(),
                MenuItem::action("Quit Jot", Quit),
            ]),
            file,
            edit,
            view,
            // A menu named "Window" also lists the open windows.
            Menu::new("Window").items([
                MenuItem::action("Minimize", Minimize),
                MenuItem::action("Zoom", Zoom),
            ]),
            Menu::new("Help").items([MenuItem::action("Keyboard Shortcuts", OpenSettings)]),
        ]
    } else {
        vec![
            file,
            edit,
            view,
            Menu::new("Help").items([
                MenuItem::action("Keyboard Shortcuts", OpenSettings),
                MenuItem::separator(),
                MenuItem::action("About Jot", OpenSettings),
            ]),
        ]
    }
}
