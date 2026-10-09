//! Several windows in one jot, on GPUI's test platform.

// Named one by one: `gpui_kit::*` would bring in GPUI's `test` attribute
// in place of the built-in one.
use super::{Launch, handle, jot_app, new_document, open_window};
use crate::actions::{NewTab, NewWindow, ToggleLineNumbers};
use crate::autocomplete::SharedVocabulary;
use crate::chrome;
use crate::state::{AppState, Settings, WindowState};
use gpui_kit::component::input::{Keymap, TextareaState};
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::path::{Path, PathBuf};

/// jot's app-wide state, with settings that aren't saved.
fn start(cx: &mut TestAppContext) -> Entity<AppState> {
    cx.update(|cx| {
        gpui_kit::init(cx);
        let settings = Settings::default();
        let app_state = AppState::init_for_test(settings.clone(), SharedVocabulary::new(), cx);
        crate::actions::init(&settings, cx);
        app_state
    })
}

fn open(paths: Vec<PathBuf>, cx: &mut TestAppContext) -> AnyWindowHandle {
    let window = cx.update(|cx| open_window(paths, cx)).unwrap();
    settle(cx);
    window
}

/// Runs what is pending and draws every window.
fn settle(cx: &mut TestAppContext) {
    cx.run_until_parked();
    for window in cx.windows() {
        cx.update_window(window, |_, window, cx| window.draw(cx).clear(cx))
            .unwrap();
    }
    cx.run_until_parked();
}

fn window_state(window: AnyWindowHandle, cx: &mut TestAppContext) -> Entity<WindowState> {
    cx.update_window(window, |_, window, cx| {
        jot_app(window, cx).unwrap().read(cx).window_state().clone()
    })
    .unwrap()
}

/// The titles of `window`'s tabs, and the active one's.
fn tabs(window: AnyWindowHandle, cx: &mut TestAppContext) -> (Vec<String>, String) {
    let state = window_state(window, cx);
    cx.read(|cx| {
        let state = state.read(cx);
        let titles: Vec<String> = state
            .documents
            .iter()
            .map(|doc| doc.read(cx).title.to_string())
            .collect();
        let active = titles[state.active_index].clone();
        (titles, active)
    })
}

fn each_editor<T>(
    window: AnyWindowHandle,
    cx: &mut TestAppContext,
    read: impl Fn(&TextareaState) -> T,
) -> Vec<T> {
    let state = window_state(window, cx);
    cx.read(|cx| {
        state
            .read(cx)
            .documents
            .iter()
            .map(|doc| read(doc.read(cx).editor_state.read(cx)))
            .collect()
    })
}

/// A folder of text files, removed afterwards.
struct Files(PathBuf);

impl Files {
    fn new(name: &str, files: &[&str]) -> Self {
        let dir =
            std::env::temp_dir().join(format!("jot-windows-test-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for file in files {
            std::fs::write(dir.join(file), format!("{file}\n")).unwrap();
        }
        Self(dir)
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Files {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[gpui_kit::test]
fn new_window_shares_the_settings_and_the_vocabulary(cx: &mut TestAppContext) {
    let app_state = start(cx);
    let first = open(Vec::new(), cx);

    cx.dispatch_action(first, NewWindow);
    settle(cx);
    let windows = cx.windows();
    assert_eq!(windows.len(), 2);
    let second = windows.into_iter().find(|window| *window != first).unwrap();

    // Each window has its own tabs, and both have the one AppState.
    assert_eq!(tabs(first, cx).0, ["Untitled"]);
    assert_eq!(tabs(second, cx).0, ["Untitled"]);
    let (first_state, second_state) = (window_state(first, cx), window_state(second, cx));
    cx.read(|cx| {
        assert_eq!(first_state.read(cx).app_state, app_state);
        assert_eq!(second_state.read(cx).app_state, app_state);
        assert_eq!(AppState::global(cx), app_state);
    });
}

#[gpui_kit::test]
fn a_setting_changed_in_one_window_reaches_the_others(cx: &mut TestAppContext) {
    let app_state = start(cx);
    let first = open(Vec::new(), cx);
    let second = open(Vec::new(), cx);
    cx.dispatch_action(second, NewTab);
    settle(cx);
    assert_eq!(
        each_editor(second, cx, |editor| editor
            .presentation()
            .has_line_numbers()),
        [true, true]
    );

    // The View menu's Line Numbers, in the first window.
    cx.dispatch_action(first, ToggleLineNumbers);
    settle(cx);
    assert!(!cx.read(|cx| app_state.read(cx).settings.line_numbers));
    assert_eq!(
        each_editor(first, cx, |editor| editor.presentation().has_line_numbers()),
        [false]
    );
    assert_eq!(
        each_editor(second, cx, |editor| editor
            .presentation()
            .has_line_numbers()),
        [false, false]
    );

    // The settings page's keybindings.
    app_state.update(cx, |state, cx| state.set_keymap(Keymap::Vim, cx));
    settle(cx);
    assert_eq!(
        each_editor(first, cx, |editor| editor.current_keymap()),
        [Keymap::Vim]
    );
    assert_eq!(
        each_editor(second, cx, |editor| editor.current_keymap()),
        [Keymap::Vim, Keymap::Vim]
    );

    // A window opened later starts with them.
    let third = open(Vec::new(), cx);
    assert_eq!(
        each_editor(third, cx, |editor| editor.presentation().has_line_numbers()),
        [false]
    );
    assert_eq!(
        each_editor(third, cx, |editor| editor.current_keymap()),
        [Keymap::Vim]
    );
}

#[gpui_kit::test]
fn closing_a_window_leaves_the_other_windows_tabs(cx: &mut TestAppContext) {
    start(cx);
    let files = Files::new("close", &["a.txt", "b.txt"]);
    let first = open(vec![files.path("a.txt")], cx);
    let second = open(vec![files.path("b.txt")], cx);
    cx.dispatch_action(second, NewTab);
    settle(cx);

    // A document with unsaved changes asks first, and nothing closes.
    let typed = window_state(first, cx);
    cx.update_window(first, |_, window, cx| {
        let editor = typed.read(cx).documents[0].read(cx).editor_state.clone();
        editor.update(cx, |editor, cx| editor.set_value("changed", window, cx));
        assert!(!chrome::can_close(window, cx));
    })
    .unwrap();
    settle(cx);
    assert_eq!(cx.windows().len(), 2);

    // Without unsaved changes, the window closes alone.
    let third = open(Vec::new(), cx);
    cx.update_window(third, |_, window, cx| chrome::close_window(window, cx))
        .unwrap();
    settle(cx);
    assert!(!cx.windows().contains(&third));
    assert_eq!(cx.windows().len(), 2);
    assert_eq!(
        tabs(second, cx),
        (vec!["b.txt".into(), "Untitled".into()], "Untitled".into())
    );
    assert_eq!(tabs(first, cx).0, ["a.txt"]);
}

#[gpui_kit::test]
fn files_open_in_the_most_recently_used_window(cx: &mut TestAppContext) {
    let app_state = start(cx);
    let files = Files::new("recent", &["a.txt", "b.txt", "c.txt"]);
    let first = open(Vec::new(), cx);
    let second = open(Vec::new(), cx);
    app_state.update(cx, |state, _| state.window_activated(first));

    // The first window's empty Untitled document gives its tab up.
    cx.update(|cx| {
        handle(
            Launch::Files(vec![files.path("a.txt"), files.path("b.txt")]),
            cx,
        )
    });
    settle(cx);
    assert_eq!(
        tabs(first, cx),
        (vec!["a.txt".into(), "b.txt".into()], "b.txt".into())
    );
    assert_eq!(tabs(second, cx).0, ["Untitled"]);

    // A file that is open comes to the front in its own window.
    app_state.update(cx, |state, _| state.window_activated(second));
    cx.update(|cx| handle(Launch::Files(vec![files.path("a.txt")]), cx));
    settle(cx);
    assert_eq!(tabs(first, cx).1, "a.txt");
    assert_eq!(tabs(second, cx).0, ["Untitled"]);

    // Missing files open nothing, and the rest still open.
    let missing = files.path("missing.txt");
    cx.update(|cx| handle(Launch::Files(vec![missing, files.path("c.txt")]), cx));
    settle(cx);
    assert_eq!(tabs(second, cx), (vec!["c.txt".into()], "c.txt".into()));
    assert_eq!(cx.windows().len(), 2);
}

#[gpui_kit::test]
fn a_launch_without_files_opens_a_window(cx: &mut TestAppContext) {
    start(cx);
    open(Vec::new(), cx);
    cx.update(|cx| handle(Launch::Files(Vec::new()), cx));
    settle(cx);
    assert_eq!(cx.windows().len(), 2);
}

#[gpui_kit::test]
fn a_new_document_opens_in_the_most_recently_used_window(cx: &mut TestAppContext) {
    let app_state = start(cx);
    // With no window, it opens one.
    cx.update(new_document);
    settle(cx);
    let first = cx.windows()[0];
    assert_eq!(tabs(first, cx).0, ["Untitled"]);

    let second = open(Vec::new(), cx);
    app_state.update(cx, |state, _| state.window_activated(first));
    cx.update(new_document);
    settle(cx);
    assert_eq!(
        tabs(first, cx),
        (
            vec!["Untitled".into(), "Untitled 2".into()],
            "Untitled 2".into()
        )
    );
    assert_eq!(tabs(second, cx).0, ["Untitled"]);
    assert_eq!(cx.windows().len(), 2);
}

#[gpui_kit::test]
fn a_window_opened_with_files_has_only_them(cx: &mut TestAppContext) {
    start(cx);
    let files = Files::new("start", &["a.txt"]);
    let window = open(
        vec![files.path("a.txt"), Path::new("/nowhere/x.txt").into()],
        cx,
    );
    assert_eq!(tabs(window, cx), (vec!["a.txt".into()], "a.txt".into()));

    // With none that opens, it starts with a new document.
    let window = open(vec![files.path("missing.txt")], cx);
    assert_eq!(tabs(window, cx).0, ["Untitled"]);
}
