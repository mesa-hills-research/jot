use super::{AppState, Document, EditorOptions, Save, Settings, on_own_thread};
use crate::actions::*;
use crate::components::View;
use futures::FutureExt as _;
use gpui_kit::component::input::{Keymap, Position};
use gpui_kit::component::{WindowExt as _, notification::Notification};
use gpui_kit::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, Subscription, Window,
};
use std::io;
use std::path::{Path, PathBuf};

// Subscribers read only some of the indices.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum WindowEvent {
    TabAdded(usize),
    TabRemoved(usize),
    ActiveTabChanged(usize),
    DirtyStateChanged,
    ViewChanged(View),
}

/// The settings a window gives its documents' editors.
#[derive(Clone, Copy, PartialEq)]
struct EditorSettings {
    word_wrap: bool,
    line_numbers: bool,
    spell_check: bool,
    keymap: Keymap,
    smooth_caret: bool,
}

impl EditorSettings {
    fn of(settings: &Settings) -> Self {
        Self {
            word_wrap: settings.word_wrap,
            line_numbers: settings.line_numbers,
            spell_check: settings.spell_check,
            keymap: settings.keymap,
            smooth_caret: settings.smooth_caret,
        }
    }
}

/// One window's state: its documents, each in a tab. The settings and the
/// word suggestions are in the [`AppState`] every window shares.
pub struct WindowState {
    pub app_state: Entity<AppState>,
    pub documents: Vec<Entity<Document>>,
    pub active_index: usize,
    untitled_counter: u32,
    pub focus_handle: FocusHandle,
    pub current_view: View,
    /// Whether the window is asking about unsaved changes on its way to
    /// closing.
    pub is_closing_window: bool,
    /// The settings the documents' editors have.
    applied: EditorSettings,
    _settings: Subscription,
    _saves_on_quit: Subscription,
}

impl EventEmitter<WindowEvent> for WindowState {}

impl Focusable for WindowState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl WindowState {
    pub fn new(app_state: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let applied = EditorSettings::of(&app_state.read(cx).settings);
        let settings = cx.observe_in(&app_state, window, |this, _, window, cx| {
            this.apply_settings(window, cx);
        });
        // Quit waits for the saves before it gets here. Logging off or
        // shutting down may not, and then they get what time quitting gives.
        let saves_on_quit = cx.on_app_quit(|this: &mut Self, cx| {
            let saves = this.pending_saves(cx);
            async move {
                for save in saves {
                    let _ = save.await;
                }
            }
        });
        Self {
            app_state,
            documents: Vec::new(),
            active_index: 0,
            untitled_counter: 1,
            focus_handle: cx.focus_handle(),
            current_view: View::Editor,
            is_closing_window: false,
            applied,
            _settings: settings,
            _saves_on_quit: saves_on_quit,
        }
    }

    /// Gives the documents' editors the settings, changing only what
    /// changed: a new keymap starts afresh, and word wrap scrolls back to
    /// the left.
    fn apply_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let settings = EditorSettings::of(&self.app_state.read(cx).settings);
        let applied = std::mem::replace(&mut self.applied, settings);
        if settings == applied {
            return;
        }
        for doc in &self.documents {
            let editor_state = doc.read(cx).editor_state.clone();
            editor_state.update(cx, |state, cx| {
                if settings.word_wrap != applied.word_wrap {
                    state.set_soft_wrap(settings.word_wrap, window, cx);
                }
                if settings.line_numbers != applied.line_numbers {
                    state.set_line_number(settings.line_numbers, window, cx);
                }
                if settings.spell_check != applied.spell_check {
                    state.set_spell_checking(settings.spell_check, cx);
                }
                if settings.keymap != applied.keymap {
                    state.set_keymap(settings.keymap, cx);
                }
                if settings.smooth_caret != applied.smooth_caret {
                    state.set_smooth_caret(settings.smooth_caret, cx);
                }
            });
        }
        cx.notify();
    }

    /// The settings, which every window shares.
    pub fn settings<'a>(&self, cx: &'a App) -> &'a Settings {
        &self.app_state.read(cx).settings
    }

    /// How a new document's editor starts out.
    fn editor_options(&self, cx: &App) -> EditorOptions {
        let app_state = self.app_state.read(cx);
        let settings = &app_state.settings;
        EditorOptions {
            word_wrap: settings.word_wrap,
            line_numbers: settings.line_numbers,
            spell_check: settings.spell_check,
            keymap: settings.keymap,
            smooth_caret: settings.smooth_caret,
            writing: app_state.writing.clone(),
        }
    }

    /// Stops closing the window, and quitting, after the user cancelled.
    pub fn cancel_closing(&mut self, cx: &mut App) {
        self.is_closing_window = false;
        self.app_state.update(cx, |state, _| state.quitting = false);
    }

    pub fn new_untitled_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.next_untitled_number();
        let options = self.editor_options(cx);
        let document = cx.new(|cx| Document::new_untitled(number, &options, window, cx));
        self.add_document(document, cx);
    }

    fn next_untitled_number(&mut self) -> Option<u32> {
        let number = (self.untitled_counter > 1).then_some(self.untitled_counter);
        self.untitled_counter += 1;
        number
    }

    fn add_document(&mut self, document: Entity<Document>, cx: &mut Context<Self>) {
        self.documents.push(document);
        self.active_index = self.documents.len() - 1;

        cx.emit(WindowEvent::TabAdded(self.active_index));
        cx.emit(WindowEvent::ActiveTabChanged(self.active_index));
        cx.notify();
    }

    /// The tab of `doc`, if it is still open.
    pub fn index_of(&self, doc: &Entity<Document>) -> Option<usize> {
        self.documents.iter().position(|open| open == doc)
    }

    /// The tab of the file at `path`, if it is open.
    pub fn tab_for(&self, path: &Path, cx: &App) -> Option<usize> {
        self.documents.iter().position(|doc| {
            doc.read(cx)
                .path
                .as_deref()
                .is_some_and(|open| same_file(open, path))
        })
    }

    /// Opens the file at `path` in a new tab, or switches to its tab when it
    /// is open already. An error says why it couldn't be opened.
    pub fn open_file(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        if let Some(index) = self.tab_for(&path, cx) {
            self.switch_to_tab(index, cx);
            return Ok(());
        }
        let content = read_text(&path)?;
        #[cfg(target_os = "windows")]
        crate::windows_shell::file_used(&path, cx);
        let options = self.editor_options(cx);
        let document = cx.new(|cx| Document::from_path(path, content, &options, window, cx));
        self.add_document(document, cx);
        Ok(())
    }

    /// Opens the files at `paths`, each in a tab, and shows the last one. A
    /// window holding only an empty Untitled document gives its tab up to
    /// them. Returns why some couldn't be opened.
    pub fn open_paths(
        &mut self,
        paths: Vec<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<String> {
        let blank = self.lone_blank_document(cx);
        let mut errors = Vec::new();
        let mut opened = false;
        for path in paths {
            match self.open_file(path, window, cx) {
                Ok(()) => opened = true,
                Err(error) => errors.push(error),
            }
        }
        if opened {
            if let Some(index) = blank.and_then(|blank| {
                self.documents
                    .iter()
                    .position(|document| *document == blank)
            }) {
                self.remove_tab(index, window, cx);
            }
            if self.current_view != View::Editor {
                self.show_editor(cx);
            }
        }
        errors
    }

    /// The document, when the window has only one and it is a new one with
    /// nothing typed in it.
    fn lone_blank_document(&self, cx: &App) -> Option<Entity<Document>> {
        let [document] = self.documents.as_slice() else {
            return None;
        };
        let doc = document.read(cx);
        (doc.path.is_none() && doc.content(cx).is_empty()).then(|| document.clone())
    }

    pub fn open_as_template(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        let content = read_text(&path)?;
        self.new_from_template(content, window, cx);
        Ok(())
    }

    fn new_from_template(&mut self, content: String, window: &mut Window, cx: &mut Context<Self>) {
        let number = self.next_untitled_number();
        let options = self.editor_options(cx);
        let document = cx.new(|cx| Document::from_template(content, number, &options, window, cx));
        self.add_document(document, cx);
    }

    /// Saves the active document to its file, or to the one the Save As
    /// it's being saved by goes to. Returns `false` for a document without
    /// one, which needs Save As.
    pub fn save_active_document(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(doc) = self.active_document().cloned() else {
            return false;
        };
        let Some(path) = doc.read(cx).save_path() else {
            return false;
        };
        self.save_document(doc, path, window, cx);
        true
    }

    /// Saves `doc`'s text as it is now to `path`, replacing the file whole,
    /// on a thread of its own and after the document's earlier saves. Once
    /// the file has the text, the document is saved there, with unsaved
    /// changes only if the text changed meanwhile. A save that fails says
    /// why in the window and leaves the file as it was. The document's
    /// [`Document::pending_save`] ends with the save.
    pub fn save_document(
        &mut self,
        doc: Entity<Document>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = doc.read(cx).text(cx);
        let previous = doc.read(cx).pending_save();
        let write = self.app_state.read(cx).file_writer.clone();
        let state = cx.weak_entity();
        let save = window.spawn(cx, {
            let (doc, path) = (doc.clone(), path.clone());
            async move |cx| {
                if let Some(previous) = previous {
                    let _ = previous.await;
                }
                let written = {
                    let (path, text) = (path.clone(), text.clone());
                    on_own_thread(move || write(&path, text.to_string().as_bytes())).await
                };
                let saved = written.map_err(|error| {
                    format!(
                        "Couldn\u{2019}t save \u{201c}{}\u{201d}. {error}",
                        path.display()
                    )
                });
                doc.update(cx, |doc, cx| doc.save_ended(&path, text, saved.is_ok(), cx));
                let shown = cx.update(|window, cx| {
                    #[cfg(target_os = "windows")]
                    if saved.is_ok() {
                        crate::windows_shell::file_used(&path, cx);
                    }
                    state.update(cx, |state, cx| state.save_ended(&saved, window, cx))
                });
                if !matches!(shown, Ok(Ok(())))
                    && let Err(error) = &saved
                {
                    log::error!("{error}");
                }
                saved
            }
        });
        doc.update(cx, |doc, _| doc.save_started(path, save.shared()));
    }

    /// Tells the window a save of one of its documents ended.
    fn save_ended(
        &mut self,
        saved: &Result<(), String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match saved {
            Ok(()) => self.app_state.read(cx).save_vocabulary_in_background(cx),
            Err(error) => window.push_notification(Notification::error(error.clone()), cx),
        }
        cx.emit(WindowEvent::DirtyStateChanged);
        cx.notify();
    }

    pub fn force_close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.documents.len() {
            return;
        }
        self.app_state.read(cx).save_vocabulary_in_background(cx);
        self.remove_tab(index, window, cx);
    }

    fn remove_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.documents.len() <= 1 {
            self.documents.remove(index);
            self.new_untitled_document(window, cx);
            self.active_index = 0;
        } else {
            self.documents.remove(index);
            if self.active_index >= self.documents.len() {
                self.active_index = self.documents.len() - 1;
            } else if self.active_index > index {
                self.active_index -= 1;
            }
        }

        cx.emit(WindowEvent::TabRemoved(index));
        cx.emit(WindowEvent::ActiveTabChanged(self.active_index));
        cx.notify();
    }

    pub fn switch_to_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.documents.len() && index != self.active_index {
            self.active_index = index;
            cx.emit(WindowEvent::ActiveTabChanged(index));
            cx.notify();
        }
    }

    pub fn next_tab(&mut self, cx: &mut Context<Self>) {
        if !self.documents.is_empty() {
            let next = (self.active_index + 1) % self.documents.len();
            self.switch_to_tab(next, cx);
        }
    }

    pub fn previous_tab(&mut self, cx: &mut Context<Self>) {
        if !self.documents.is_empty() {
            let prev = if self.active_index == 0 {
                self.documents.len() - 1
            } else {
                self.active_index - 1
            };
            self.switch_to_tab(prev, cx);
        }
    }

    pub fn active_document(&self) -> Option<&Entity<Document>> {
        self.documents.get(self.active_index)
    }

    /// The saves of the window's documents that haven't ended, the last of
    /// each document's.
    pub fn pending_saves(&self, cx: &App) -> Vec<Save> {
        self.documents
            .iter()
            .filter_map(|doc| doc.read(cx).pending_save())
            .collect()
    }

    pub fn has_dirty_documents(&self, cx: &App) -> bool {
        self.documents.iter().any(|d| d.read(cx).dirty)
    }

    pub fn refresh_all_dirty_states(&self, cx: &mut Context<Self>) {
        for doc in &self.documents {
            doc.update(cx, |doc, cx| {
                doc.check_dirty(cx);
            });
        }
    }

    pub fn show_settings(&mut self, cx: &mut Context<Self>) {
        self.current_view = View::Settings;
        cx.emit(WindowEvent::ViewChanged(View::Settings));
        cx.notify();
    }

    /// Moves keyboard focus to the active document.
    pub fn focus_active_editor(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(doc) = self.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            editor_state.update(cx, |state, cx| state.focus(window, cx));
        }
    }

    pub fn show_editor(&mut self, cx: &mut Context<Self>) {
        self.current_view = View::Editor;
        cx.emit(WindowEvent::ViewChanged(View::Editor));
        cx.notify();
    }

    pub fn goto_line(&mut self, line: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(doc) = self.active_document() {
            let editor_state = doc.read(cx).editor_state.clone();
            let content = doc.read(cx).content(cx);
            let total_lines = content.lines().count();

            let target_line = line.saturating_sub(1).min(total_lines.saturating_sub(1));

            editor_state.update(cx, |state, cx| {
                state.set_cursor_position(
                    Position {
                        line: target_line as u32,
                        character: 0,
                    },
                    window,
                    cx,
                );
            });
        }
    }

    pub fn on_new_tab(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.new_untitled_document(window, cx);
    }

    pub fn on_open_settings(&mut self, _: &OpenSettings, _: &mut Window, cx: &mut Context<Self>) {
        self.show_settings(cx);
    }

    pub fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.next_tab(cx);
    }

    pub fn on_previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.previous_tab(cx);
    }
}

/// Whether `a` and `b` name the same file, such as through a link or, on
/// Windows, in a different case.
pub fn same_file(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (std::fs::canonicalize(a), std::fs::canonicalize(b)),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Reads the text file at `path`, or says why it can't be opened.
pub fn read_text(path: &Path) -> Result<String, String> {
    let name = format!("\u{201c}{}\u{201d}", path.display());
    if path.is_dir() {
        return Err(format!("{name} is a folder."));
    }
    std::fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => format!("{name} doesn\u{2019}t exist."),
        io::ErrorKind::InvalidData => {
            format!("{name} isn\u{2019}t a UTF-8 text file. Jot only opens UTF-8 text files.")
        }
        _ => format!("Couldn\u{2019}t open {name}. {error}"),
    })
}
