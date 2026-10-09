use crate::autocomplete::{JotCompletionProvider, SharedVocabulary};
use crate::spell::DocumentSpelling;
use gpui_kit::component::input::{SuggestionOptions, TextareaState};
use gpui_kit::{AppContext, Context, Entity, SharedString, Window};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

/// How a new document's editor starts out, from the settings.
#[derive(Clone)]
pub struct EditorOptions {
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub spell_check: bool,
    pub shared_vocab: Rc<RefCell<SharedVocabulary>>,
    pub autocomplete_enabled: Rc<Cell<bool>>,
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub title: SharedString,
    pub dirty: bool,
    pub editor_state: Entity<TextareaState>,
    content_hash: u64,
}

impl Document {
    pub fn new_untitled(
        number: Option<u32>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            path: None,
            title: untitled_title(number),
            dirty: false,
            editor_state: Self::build_editor_state(None, options, window, cx),
            content_hash: 0,
        }
    }

    pub fn from_path(
        path: PathBuf,
        content: String,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
            .into();
        let content_hash = Self::hash_content(&content);
        Self {
            path: Some(path),
            title,
            dirty: false,
            editor_state: Self::build_editor_state(Some(content), options, window, cx),
            content_hash,
        }
    }

    pub fn from_template(
        content: String,
        number: Option<u32>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let dirty = !content.is_empty();
        Self {
            path: None,
            title: untitled_title(number),
            dirty,
            editor_state: Self::build_editor_state(Some(content), options, window, cx),
            content_hash: 0,
        }
    }

    /// Builds the editor state for a document: GPUI Kit's text editor, with
    /// jot's spell checker and word suggestions shown as ghost text.
    fn build_editor_state(
        content: Option<String>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<TextareaState> {
        let dictionary = options.shared_vocab.borrow().dictionary();
        let completions = Rc::new(JotCompletionProvider::new(
            options.shared_vocab.clone(),
            options.autocomplete_enabled.clone(),
        ));
        cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .text_editor()
                .soft_wrap(options.word_wrap)
                .line_number(options.line_numbers)
                .suggestion_provider(completions)
                .suggestion_options(SuggestionOptions::default().menu(false).inline(true));
            if dictionary.is_loaded() {
                state = state.spell_checker(Rc::new(DocumentSpelling::new(dictionary)));
            }
            if let Some(content) = content {
                state = state.default_value(content);
            }
            state.set_spell_checking(options.spell_check, cx);
            state
        })
    }

    pub fn content(&self, cx: &gpui_kit::App) -> String {
        self.editor_state.read(cx).value().to_string()
    }

    pub fn check_dirty(&mut self, cx: &gpui_kit::App) {
        let current_content = self.content(cx);
        let current_hash = Self::hash_content(&current_content);

        if self.path.is_none() {
            self.dirty = !current_content.is_empty();
        } else {
            self.dirty = current_hash != self.content_hash;
        }
    }

    pub fn mark_saved(&mut self, cx: &gpui_kit::App) {
        let content = self.content(cx);
        self.content_hash = Self::hash_content(&content);
        self.dirty = false;

        if let Some(path) = &self.path {
            self.title = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "Unknown".to_string())
                .into();
        }
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
            .into();
        self.path = Some(path);
    }

    fn hash_content(content: &str) -> u64 {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        content.hash(&mut hasher);
        hasher.finish()
    }
}

fn untitled_title(number: Option<u32>) -> SharedString {
    match number {
        Some(n) if n > 1 => format!("Untitled {}", n).into(),
        _ => "Untitled".into(),
    }
}
