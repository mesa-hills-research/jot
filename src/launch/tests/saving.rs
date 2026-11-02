//! Saving documents on threads of their own, while the window goes on.

use super::{Files, open, settle, tabs, window_state};
use crate::actions::{CloseTab, Save};
use crate::chrome;
use crate::components::{close_tab_with_prompt, save_and_close};
use crate::state::{AppState, FileWriter, Save as PendingSave};
use gpui_kit::component::WindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use std::io;
use std::path::Path;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::{self, ThreadId};
use std::time::Duration;

/// Stands in for jot's file writer. It holds each write until the test lets
/// it go, and tells the test what each write was given and on which thread
/// it ran.
struct Gate {
    started: mpsc::Receiver<(String, ThreadId)>,
    release: mpsc::Sender<io::Result<()>>,
}

impl Gate {
    /// Puts the gate in the place of `app_state`'s writer. A write let go
    /// with `Ok` writes the file.
    fn install(app_state: &Entity<AppState>, cx: &mut TestAppContext) -> Self {
        let (started_sender, started) = mpsc::channel();
        let (release, released) = mpsc::channel::<io::Result<()>>();
        let released = Mutex::new(released);
        let writer: FileWriter = Arc::new(move |path, data| {
            let text = String::from_utf8_lossy(data).into_owned();
            let _ = started_sender.send((text, thread::current().id()));
            // A write held on the UI thread would hold up the test too, so
            // it gives up after a while.
            let let_go = released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap_or_else(|_| Err(io::Error::other("The test never let the write go")));
            let backups = path.parent().unwrap().join("backups");
            let_go.and_then(|()| crate::file_write::write_whole(path, data, &backups))
        });
        cx.update(|cx| app_state.update(cx, |state, _| state.file_writer = writer));
        // The writes end on threads of their own, which wake the test's.
        cx.executor().allow_parking();
        Self { started, release }
    }

    /// The text of the next write once it has started, and whether it runs
    /// on the UI thread, which is the test's.
    fn next_write(&self) -> (String, bool) {
        let (text, thread) = self
            .started
            .recv_timeout(Duration::from_secs(10))
            .expect("no write started");
        (text, thread == thread::current().id())
    }

    fn no_write_started(&self) -> bool {
        matches!(self.started.try_recv(), Err(TryRecvError::Empty))
    }

    fn let_go(&self, result: io::Result<()>) {
        self.release.send(result).unwrap();
    }
}

/// Replaces the text of `window`'s first document.
fn type_text(window: AnyWindowHandle, text: &'static str, cx: &mut TestAppContext) {
    type_text_in(window, 0, text, cx);
}

/// Replaces the text of `window`'s document in tab `index`.
fn type_text_in(
    window: AnyWindowHandle,
    index: usize,
    text: &'static str,
    cx: &mut TestAppContext,
) {
    let state = window_state(window, cx);
    cx.update_window(window, |_, window, cx| {
        let editor = state.read(cx).documents[index]
            .read(cx)
            .editor_state
            .clone();
        editor.update(cx, |editor, cx| editor.set_value(text, window, cx));
    })
    .unwrap();
}

/// Whether `window`'s first document has unsaved changes.
fn is_dirty(window: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    let state = window_state(window, cx);
    cx.update(|cx| {
        let document = state.read(cx).documents[0].clone();
        document.update(cx, |document, cx| {
            document.check_dirty(cx);
            document.dirty
        })
    })
}

fn notifications(window: AnyWindowHandle, cx: &mut TestAppContext) -> usize {
    cx.update_window(window, |_, window, cx| window.notifications(cx).len())
        .unwrap()
}

fn has_dialog(window: AnyWindowHandle, cx: &mut TestAppContext) -> bool {
    cx.update_window(window, |_, window, cx| window.has_active_dialog(cx))
        .unwrap()
}

/// The save of `window`'s first document that ends last, if one hasn't.
fn pending_save(window: AnyWindowHandle, cx: &mut TestAppContext) -> Option<PendingSave> {
    let state = window_state(window, cx);
    cx.read(|cx| state.read(cx).documents[0].read(cx).pending_save())
}

/// Waits for `save` to end, and returns how it ended.
fn wait_for(save: PendingSave, cx: &mut TestAppContext) -> Result<(), String> {
    let ended = cx.foreground_executor().block_test(save);
    settle(cx);
    ended
}

/// Waits for the saves of `window`'s documents to end.
fn wait_for_saves(window: AnyWindowHandle, cx: &mut TestAppContext) {
    let state = window_state(window, cx);
    for save in cx.read(|cx| state.read(cx).pending_saves(cx)) {
        let _ = wait_for(save, cx);
    }
    settle(cx);
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

#[gpui_kit::test]
fn a_save_runs_off_the_ui_thread_and_text_typed_meanwhile_stays_unsaved(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let gate = Gate::install(&app_state, cx);
    let files = Files::new("save-off-thread", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);

    type_text(window, "first", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    let (text, on_ui_thread) = gate.next_write();
    assert_eq!(text, "first");
    assert!(!on_ui_thread, "the file was written on the UI thread");

    // The window takes typing while the write is held up.
    assert_eq!(read(&path), "notes.txt\n");
    type_text(window, "second", cx);
    assert!(is_dirty(window, cx));

    gate.let_go(Ok(()));
    wait_for_saves(window, cx);
    assert_eq!(read(&path), "first");
    assert!(
        is_dirty(window, cx),
        "text typed during the save shows as saved"
    );
    assert_eq!(notifications(window, cx), 0);

    // Typing back what was saved leaves nothing unsaved.
    type_text(window, "first", cx);
    assert!(!is_dirty(window, cx));
}

#[gpui_kit::test]
fn two_saves_in_a_row_write_in_turn(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let gate = Gate::install(&app_state, cx);
    let files = Files::new("saves-in-a-row", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);

    type_text(window, "one", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    assert_eq!(gate.next_write().0, "one");
    let first = pending_save(window, cx).expect("the first save ended at once");

    type_text(window, "two", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    assert!(
        gate.no_write_started(),
        "the second save started before the first ended"
    );

    gate.let_go(Ok(()));
    assert_eq!(wait_for(first, cx), Ok(()));
    assert_eq!(read(&path), "one");
    assert_eq!(gate.next_write().0, "two");
    gate.let_go(Ok(()));
    wait_for_saves(window, cx);
    assert_eq!(read(&path), "two");
    assert!(!is_dirty(window, cx));
}

#[gpui_kit::test]
fn closing_a_tab_waits_for_its_save(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let gate = Gate::install(&app_state, cx);
    let files = Files::new("close-tab-saving", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);

    type_text(window, "closing", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    gate.next_write();
    cx.dispatch_action(window, CloseTab);
    settle(cx);
    assert_eq!(tabs(window, cx).0, ["notes.txt"]);
    assert!(
        !has_dialog(window, cx),
        "closing asked about changes that were being saved"
    );

    gate.let_go(Ok(()));
    wait_for_saves(window, cx);
    assert_eq!(read(&path), "closing");
    assert_eq!(tabs(window, cx).0, ["Untitled"]);
}

#[gpui_kit::test]
fn closing_a_window_waits_for_its_saves(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let gate = Gate::install(&app_state, cx);
    let files = Files::new("close-window-saving", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);
    let other = open(Vec::new(), cx);

    type_text(window, "closing", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    gate.next_write();
    let save = pending_save(window, cx).expect("the save ended at once");
    cx.update_window(window, |_, window, cx| chrome::close_window(window, cx))
        .unwrap();
    settle(cx);
    assert!(cx.windows().contains(&window));
    assert!(
        !has_dialog(window, cx),
        "closing asked about changes that were being saved"
    );

    gate.let_go(Ok(()));
    assert_eq!(wait_for(save, cx), Ok(()));
    assert_eq!(read(&path), "closing");
    assert!(!cx.windows().contains(&window));
    assert!(cx.windows().contains(&other));
}

#[gpui_kit::test]
fn a_failed_background_save_says_why_and_keeps_the_changes(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let gate = Gate::install(&app_state, cx);
    let files = Files::new("failed-background-save", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);

    type_text(window, "unsaved", cx);
    cx.dispatch_action(window, Save);
    settle(cx);
    gate.next_write();
    let save = pending_save(window, cx).expect("the save ended at once");
    // Closing waits for the save, and asks once it fails.
    cx.dispatch_action(window, CloseTab);
    settle(cx);

    gate.let_go(Err(io::Error::other("The disk is full")));
    assert_eq!(
        wait_for(save, cx),
        Err(format!(
            "Couldn\u{2019}t save \u{201c}{}\u{201d}. The disk is full",
            path.display()
        ))
    );
    assert_eq!(notifications(window, cx), 1);
    assert_eq!(read(&path), "notes.txt\n");
    assert!(is_dirty(window, cx));
    assert!(has_dialog(window, cx));
    assert_eq!(tabs(window, cx).0, ["notes.txt"]);
}

/// Save writes the file. A save that fails says why, keeps the changes
/// unsaved and leaves the file as it was.
#[cfg(unix)]
#[gpui_kit::test]
fn a_failed_save_says_why_and_keeps_the_changes(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        // Root may write any file.
        return;
    }
    super::start(cx);
    cx.executor().allow_parking();
    let files = Files::new("failed-save", &["notes.txt"]);
    let path = files.path("notes.txt");
    let window = open(vec![path.clone()], cx);
    let type_and_save = |text: &'static str, cx: &mut TestAppContext| {
        type_text(window, text, cx);
        cx.dispatch_action(window, Save);
        settle(cx);
        wait_for_saves(window, cx);
    };

    type_and_save("saved", cx);
    assert_eq!(read(&path), "saved");
    assert!(!is_dirty(window, cx));
    assert_eq!(notifications(window, cx), 0);

    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    type_and_save("not saved", cx);
    assert_eq!(read(&path), "saved");
    assert!(is_dirty(window, cx));
    assert_eq!(notifications(window, cx), 1);
}

/// Save, in the question about unsaved changes, saves and closes the
/// document it asked about, though other tabs closed meanwhile.
#[gpui_kit::test]
fn save_in_the_question_closes_the_tab_it_asked_about(cx: &mut TestAppContext) {
    super::start(cx);
    cx.executor().allow_parking();
    let files = Files::new("save-in-question", &["a.txt", "b.txt", "c.txt"]);
    let paths = ["a.txt", "b.txt", "c.txt"].map(|name| files.path(name));
    let window = open(paths.to_vec(), cx);
    let state = window_state(window, cx);
    type_text_in(window, 1, "b changed", cx);
    type_text_in(window, 2, "c changed", cx);
    let b = cx.read(|cx| state.read(cx).documents[1].clone());

    cx.update_window(window, |_, window, cx| {
        close_tab_with_prompt(state.clone(), 1, window, cx);
    })
    .unwrap();
    settle(cx);
    assert!(has_dialog(window, cx));
    // The first tab closes while the question is open, which moves the
    // others along, and then Save is clicked.
    cx.update_window(window, |_, window, cx| {
        window.close_dialog(cx);
        state.update(cx, |state, cx| state.force_close_tab(0, window, cx));
        save_and_close(window, state.clone(), b.clone(), cx);
    })
    .unwrap();
    settle(cx);
    wait_for_saves(window, cx);

    assert_eq!(read(&paths[1]), "b changed");
    assert_eq!(tabs(window, cx).0, ["c.txt"]);
    assert!(is_dirty(window, cx));
}

/// Closing the last window, which jot quits with on Windows and Linux,
/// waits for the settings' latest write.
#[gpui_kit::test]
fn closing_the_last_window_waits_for_the_settings(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let files = Files::new("last-window-settings", &[]);
    let settings_file = files.path("settings.json");
    app_state.update(cx, |state, _| {
        state.set_settings_file(settings_file.clone())
    });
    let gate = Gate::install(&app_state, cx);
    let window = open(Vec::new(), cx);

    app_state.update(cx, |state, cx| state.toggle_word_wrap(cx));
    settle(cx);
    gate.next_write();
    let saved = cx.read(|cx| app_state.read(cx).settings_saved());
    cx.update_window(window, |_, window, cx| chrome::close_window(window, cx))
        .unwrap();
    settle(cx);
    assert!(cx.windows().contains(&window));

    gate.let_go(Ok(()));
    cx.foreground_executor().block_test(saved);
    settle(cx);
    assert!(!cx.windows().contains(&window));
    let saved = crate::state::Settings::load(&settings_file).unwrap();
    assert!(saved.word_wrap);
}

/// Quit, with no changes to ask about, waits for the settings' write. Text
/// typed while it waits is asked about, rather than lost.
#[gpui_kit::test]
fn quit_asks_about_changes_typed_while_it_waits(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let files = Files::new("quit-typed-meanwhile", &["notes.txt"]);
    app_state.update(cx, |state, _| {
        state.set_settings_file(files.path("settings.json"))
    });
    let gate = Gate::install(&app_state, cx);
    let first = open(vec![files.path("notes.txt")], cx);
    let second = open(Vec::new(), cx);
    app_state.update(cx, |state, cx| state.toggle_word_wrap(cx));
    settle(cx);
    gate.next_write();

    cx.update(chrome::quit);
    settle(cx);
    assert_eq!(cx.windows().len(), 2);
    type_text(first, "typed while quitting", cx);
    let saved = cx.read(|cx| app_state.read(cx).settings_saved());
    gate.let_go(Ok(()));
    cx.foreground_executor().block_test(saved);
    settle(cx);

    assert!(cx.windows().contains(&first));
    assert!(
        has_dialog(first, cx),
        "quitting didn't ask about the changes"
    );
    assert!(cx.windows().contains(&second));
    assert_eq!(read(&files.path("notes.txt")), "notes.txt\n");
}

/// Quit closes every window once the settings' write and the saves started
/// while it waited have ended, a window opened meanwhile too.
#[gpui_kit::test]
fn quit_waits_for_saves_started_while_it_waits(cx: &mut TestAppContext) {
    let app_state = super::start(cx);
    let files = Files::new("quit-saves-meanwhile", &["notes.txt"]);
    app_state.update(cx, |state, _| {
        state.set_settings_file(files.path("settings.json"))
    });
    let gate = Gate::install(&app_state, cx);
    open(Vec::new(), cx);
    open(Vec::new(), cx);
    app_state.update(cx, |state, cx| state.toggle_word_wrap(cx));
    settle(cx);
    gate.next_write();

    cx.update(chrome::quit);
    settle(cx);
    let opened = open(vec![files.path("notes.txt")], cx);
    type_text(opened, "saved while quitting", cx);
    cx.dispatch_action(opened, Save);
    settle(cx);
    let save = pending_save(opened, cx).expect("the save ended at once");

    let saved = cx.read(|cx| app_state.read(cx).settings_saved());
    gate.let_go(Ok(()));
    cx.foreground_executor().block_test(saved);
    settle(cx);
    assert!(
        cx.windows().contains(&opened),
        "quit didn't wait for the save"
    );
    assert_eq!(gate.next_write().0, "saved while quitting");
    gate.let_go(Ok(()));
    assert_eq!(wait_for(save, cx), Ok(()));
    settle(cx);

    assert!(cx.windows().is_empty(), "{:?}", cx.windows());
    assert_eq!(read(&files.path("notes.txt")), "saved while quitting");
}
