use super::{Document, Settings};
use crate::actions::*;
use crate::autocomplete::SharedVocabulary;
use crate::components::View;
use gpui::{
    App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable, ParentElement,
    Subscription, Window,
};
use gpui_component::input::Position;
use gpui_component::WindowExt;
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

#[derive(Clone, Debug)]
pub enum AppEvent {
    TabAdded(usize),
    TabRemoved(usize),
    ActiveTabChanged(usize),
    DirtyStateChanged,
    SettingsChanged,
    ViewChanged(View),
}

pub struct AppState {
    pub documents: Vec<Entity<Document>>,
    pub active_index: usize,
    pub settings: Settings,
    pub autocomplete_enabled: Rc<Cell<bool>>,
    pub spell_check_enabled: Rc<Cell<bool>>,
    pub shared_vocabulary: Rc<RefCell<SharedVocabulary>>,
    untitled_counter: u32,
    pub focus_handle: FocusHandle,
    pub current_view: View,
    pub search_visible: bool,
    pub replace_mode: bool,
    pub is_closing_window: bool,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<AppEvent> for AppState {}

impl Focusable for AppState {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl AppState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        let settings = Settings::load();
        let autocomplete_enabled = Rc::new(Cell::new(settings.autocomplete));
        let spell_check_enabled = Rc::new(Cell::new(settings.spell_check));
        let shared_vocabulary = Rc::new(RefCell::new(SharedVocabulary::load()));

        Self {
            documents: Vec::new(),
            active_index: 0,
            settings,
            autocomplete_enabled,
            spell_check_enabled,
            shared_vocabulary,
            untitled_counter: 1,
            focus_handle,
            current_view: View::Editor,
            search_visible: false,
            replace_mode: false,
            is_closing_window: false,
            _subscriptions: Vec::new(),
        }
    }

    pub fn apply_settings_to_all_docs(&self, window: &mut Window, cx: &mut Context<Self>) {
        let word_wrap = self.settings.word_wrap;
        let line_numbers = self.settings.line_numbers;

        for doc in &self.documents {
            let editor_state = doc.read(cx).editor_state.clone();
            editor_state.update(cx, |state, cx| {
                state.set_soft_wrap(word_wrap, window, cx);
                state.set_line_number(line_numbers, window, cx);
            });
        }
    }

    /// Updates the spell-check setting and re-runs (or clears) diagnostics
    /// on every open document.
    pub fn set_spell_check(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.spell_check = enabled;
        self.spell_check_enabled.set(enabled);
        self.settings.save();

        for doc in &self.documents {
            doc.update(cx, |doc, cx| {
                doc.schedule_spell_check(cx);
            });
        }
        cx.notify();
    }

    pub fn new_untitled_document(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let number = if self.untitled_counter == 1 {
            None
        } else {
            Some(self.untitled_counter)
        };
        self.untitled_counter += 1;

        let word_wrap = self.settings.word_wrap;
        let line_numbers = self.settings.line_numbers;
        let shared_vocab = self.shared_vocabulary.clone();
        let autocomplete_enabled = self.autocomplete_enabled.clone();
        let spell_enabled = self.spell_check_enabled.clone();

        let document = cx.new(|cx| {
            Document::new_untitled(
                number,
                word_wrap,
                line_numbers,
                shared_vocab,
                autocomplete_enabled,
                spell_enabled,
                window,
                cx,
            )
        });

        self.documents.push(document);
        self.active_index = self.documents.len() - 1;

        cx.emit(AppEvent::TabAdded(self.active_index));
        cx.emit(AppEvent::ActiveTabChanged(self.active_index));
        cx.notify();
    }

    pub fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                if !content.is_empty() && String::from_utf8(content.as_bytes().to_vec()).is_err() {
                    self.show_utf8_error(&path, window, cx);
                    return;
                }

                let word_wrap = self.settings.word_wrap;
                let line_numbers = self.settings.line_numbers;
                let shared_vocab = self.shared_vocabulary.clone();
                let autocomplete_enabled = self.autocomplete_enabled.clone();
                let spell_enabled = self.spell_check_enabled.clone();

                let document = cx.new(|cx| {
                    Document::from_path(
                        path,
                        content,
                        word_wrap,
                        line_numbers,
                        shared_vocab,
                        autocomplete_enabled,
                        spell_enabled,
                        window,
                        cx,
                    )
                });

                self.documents.push(document);
                self.active_index = self.documents.len() - 1;

                cx.emit(AppEvent::TabAdded(self.active_index));
                cx.emit(AppEvent::ActiveTabChanged(self.active_index));
                cx.notify();
            }
            Err(e) => {
                log::error!("Failed to read file: {}", e);
            }
        }
    }

    pub fn open_as_template(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match std::fs::read_to_string(&path) {
            Ok(content) => {
                if !content.is_empty() && String::from_utf8(content.as_bytes().to_vec()).is_err() {
                    self.show_utf8_error(&path, window, cx);
                    return;
                }
                self.new_from_template(content, window, cx);
            }
            Err(e) => {
                log::error!("Failed to read template file: {}", e);
            }
        }
    }

    fn new_from_template(
        &mut self,
        content: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let number = if self.untitled_counter == 1 {
            None
        } else {
            Some(self.untitled_counter)
        };
        self.untitled_counter += 1;

        let word_wrap = self.settings.word_wrap;
        let line_numbers = self.settings.line_numbers;
        let shared_vocab = self.shared_vocabulary.clone();
        let autocomplete_enabled = self.autocomplete_enabled.clone();
        let spell_enabled = self.spell_check_enabled.clone();

        let document = cx.new(|cx| {
            Document::from_template(
                content,
                number,
                word_wrap,
                line_numbers,
                shared_vocab,
                autocomplete_enabled,
                spell_enabled,
                window,
                cx,
            )
        });

        self.documents.push(document);
        self.active_index = self.documents.len() - 1;

        cx.emit(AppEvent::TabAdded(self.active_index));
        cx.emit(AppEvent::ActiveTabChanged(self.active_index));
        cx.notify();
    }

    fn show_utf8_error(&self, path: &PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let filename = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string());

        window.open_dialog(cx, move |dialog, _, _| {
            dialog.title("Cannot open file").child(format!(
                "{} is not a valid UTF-8 text file. Jot only supports UTF-8 encoded text files.",
                filename
            ))
        });
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

        self.shared_vocabulary.borrow_mut().save();

        cx.emit(AppEvent::DirtyStateChanged);
        cx.notify();
        Ok(())
    }

    pub fn document_saved(
        &mut self,
        doc: Entity<Document>,
        path: PathBuf,
        cx: &mut Context<Self>,
    ) {
        doc.update(cx, |doc, cx| {
            doc.set_path(path);
            doc.mark_saved(cx);
        });

        self.shared_vocabulary.borrow_mut().save();

        cx.emit(AppEvent::DirtyStateChanged);
        cx.notify();
    }

    pub fn force_close_tab(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index >= self.documents.len() {
            return;
        }
        self.shared_vocabulary.borrow_mut().save();
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

        cx.emit(AppEvent::TabRemoved(index));
        cx.emit(AppEvent::ActiveTabChanged(self.active_index));
        cx.notify();
    }

    pub fn switch_to_tab(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.documents.len() && index != self.active_index {
            self.active_index = index;
            cx.emit(AppEvent::ActiveTabChanged(index));
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
        cx.emit(AppEvent::ViewChanged(View::Settings));
        cx.notify();
    }

    pub fn show_editor(&mut self, cx: &mut Context<Self>) {
        self.current_view = View::Editor;
        cx.emit(AppEvent::ViewChanged(View::Editor));
        cx.notify();
    }

    pub fn toggle_search(&mut self, replace: bool, cx: &mut Context<Self>) {
        if self.search_visible && self.replace_mode == replace {
            self.search_visible = false;
        } else {
            self.search_visible = true;
            self.replace_mode = replace;
        }
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

    pub fn on_zoom_in(&mut self, _: &ZoomIn, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.zoom_in();
        self.settings.save();
        cx.emit(AppEvent::SettingsChanged);
        cx.notify();
    }

    pub fn on_zoom_out(&mut self, _: &ZoomOut, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.zoom_out();
        self.settings.save();
        cx.emit(AppEvent::SettingsChanged);
        cx.notify();
    }

    pub fn on_reset_zoom(&mut self, _: &ResetZoom, _: &mut Window, cx: &mut Context<Self>) {
        self.settings.reset_zoom();
        self.settings.save();
        cx.emit(AppEvent::SettingsChanged);
        cx.notify();
    }

    pub fn on_toggle_word_wrap(
        &mut self,
        _: &ToggleWordWrap,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.word_wrap = !self.settings.word_wrap;
        self.settings.save();
        self.apply_settings_to_all_docs(window, cx);
        cx.emit(AppEvent::SettingsChanged);
        cx.notify();
    }

    pub fn on_toggle_line_numbers(
        &mut self,
        _: &ToggleLineNumbers,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.settings.line_numbers = !self.settings.line_numbers;
        self.settings.save();
        self.apply_settings_to_all_docs(window, cx);
        cx.emit(AppEvent::SettingsChanged);
        cx.notify();
    }

    pub fn on_open_settings(
        &mut self,
        _: &OpenSettings,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.show_settings(cx);
    }

    pub fn on_find(&mut self, _: &Find, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_search(false, cx);
    }

    pub fn on_replace(&mut self, _: &Replace, _: &mut Window, cx: &mut Context<Self>) {
        self.toggle_search(true, cx);
    }

    pub fn on_next_tab(&mut self, _: &NextTab, _: &mut Window, cx: &mut Context<Self>) {
        self.next_tab(cx);
    }

    pub fn on_previous_tab(&mut self, _: &PreviousTab, _: &mut Window, cx: &mut Context<Self>) {
        self.previous_tab(cx);
    }
}