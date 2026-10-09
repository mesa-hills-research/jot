//! Global actions and keybindings for Jot.

use gpui_kit::{App, KeyBinding, actions};

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
        Undo,
        Redo,
        Cut,
        Copy,
        Paste,
        SelectAll,
        Find,
        Replace,
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
    ]
);

/// Context identifier for the main editor.
pub const EDITOR_CONTEXT: &str = "Editor";

/// Context identifier for the application.
pub const APP_CONTEXT: &str = "Jot";

/// Initialize all keybindings for the application.
pub fn init(cx: &mut App) {
    #[cfg(target_os = "macos")]
    let bindings = vec![
        KeyBinding::new("cmd-t", NewTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-n", NewWindow, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-t", NewFromTemplate, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-o", OpenFile, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-s", Save, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-shift-s", SaveAs, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-w", CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-q", Quit, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-z", Undo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-shift-z", Redo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-y", Redo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-x", Cut, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-c", Copy, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-v", Paste, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-a", SelectAll, Some(EDITOR_CONTEXT)),
        KeyBinding::new("cmd-f", Find, Some(APP_CONTEXT)),
        KeyBinding::new("cmd-h", Replace, Some(APP_CONTEXT)),
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

    #[cfg(not(target_os = "macos"))]
    let bindings = vec![
        KeyBinding::new("ctrl-t", NewTab, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-n", NewWindow, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-t", NewFromTemplate, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-o", OpenFile, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-s", Save, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-shift-s", SaveAs, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-w", CloseTab, Some(APP_CONTEXT)),
        KeyBinding::new("alt-f4", CloseWindow, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-q", Quit, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-z", Undo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-shift-z", Redo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-y", Redo, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-x", Cut, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-c", Copy, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-v", Paste, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-a", SelectAll, Some(EDITOR_CONTEXT)),
        KeyBinding::new("ctrl-f", Find, Some(APP_CONTEXT)),
        KeyBinding::new("ctrl-h", Replace, Some(APP_CONTEXT)),
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
}
