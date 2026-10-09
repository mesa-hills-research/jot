use super::{AppState, Document, EditorOptions, Settings};
use crate::actions::*;
use crate::components::View;
use gpui_kit::component::input::{Keymap, Position};
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
    /// Whether jot quits once this window has closed.
    pub quit_after_close: bool,
    /// The settings the documents' editors have.
    applied: EditorSettings,
    _settings: Subscription,
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
        Self {
            app_state,
            documents: Vec::new(),
            active_index: 0,
            untitled_counter: 1,
            focus_handle: cx.focus_handle(),
            current_view: View::Editor,
            is_closing_window: false,
            quit_after_close: false,
            applied,
            _settings: settings,
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
            shared_vocab: app_state.shared_vocabulary.clone(),
            suggestion_pacing: app_state.suggestion_pacing.clone(),
        }
    }

    /// Stops closing the window, and quitting, after the user cancelled.
    pub fn cancel_closing(&mut self) {
        self.is_closing_window = false;
        self.quit_after_close = false;
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

    pub fn save_active_document(&mut self, cx: &mut Context<Self>) -> anyhow::Result<bool> {
        let Some(doc) = self.active_document().cloned() else {
            return Ok(false);
        };

        let (path, content) = doc.update(cx, |doc, cx| (doc.path.clone(), doc.content(cx)));

        if let Some(file_path) = path {
            self.perform_save(doc, file_path, content, cx)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn perform_save(
        &mut self,
        doc: Entity<Document>,
        path: PathBuf,
        content: String,
        cx: &mut Context<Self>,
    ) -> anyhow::Result<()> {
        std::fs::write(&path, &content)?;

        doc.update(cx, |doc, cx| {
            doc.mark_saved(cx);
        });

        self.app_state.read(cx).save_vocabulary_in_background(cx);

        cx.emit(WindowEvent::DirtyStateChanged);
        cx.notify();
        Ok(())
    }

    pub fn document_saved(&mut self, doc: Entity<Document>, path: PathBuf, cx: &mut Context<Self>) {
        doc.update(cx, |doc, cx| {
            doc.set_path(path);
            doc.mark_saved(cx);
        });

        self.app_state.read(cx).save_vocabulary_in_background(cx);

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
        _ => format!("Couldn\u{2019}t open {name}: {error}"),
    })
}
