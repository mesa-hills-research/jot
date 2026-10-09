use crate::autocomplete::{JotCompletionProvider, SharedVocabulary, SuggestionPacing};
use crate::spell::DocumentSpelling;
use gpui_kit::component::input::{Keymap, SuggestionEvent, SuggestionOptions, TextareaState};
use gpui_kit::{AppContext, Context, Entity, SharedString, Subscription, Window};
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

/// How a new document's editor starts out, from the settings.
#[derive(Clone)]
pub struct EditorOptions {
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub spell_check: bool,
    pub keymap: Keymap,
    pub smooth_caret: bool,
    pub shared_vocab: Rc<RefCell<SharedVocabulary>>,
    /// The word suggestions' mode, and how the user has been answering
    /// them, shared by every document.
    pub suggestion_pacing: Rc<SuggestionPacing>,
}

pub struct Document {
    pub path: Option<PathBuf>,
    pub title: SharedString,
    pub dirty: bool,
    pub editor_state: Entity<TextareaState>,
    content_hash: u64,
    /// Tells the word suggestions which ones the user took or refused.
    _suggestion_feedback: Subscription,
}

impl Document {
    pub fn new_untitled(
        number: Option<u32>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (editor_state, suggestion_feedback) =
            Self::build_editor_state(None, options, window, cx);
        Self {
            path: None,
            title: untitled_title(number),
            dirty: false,
            editor_state,
            content_hash: 0,
            _suggestion_feedback: suggestion_feedback,
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
        let (editor_state, suggestion_feedback) =
            Self::build_editor_state(Some(content), options, window, cx);
        Self {
            path: Some(path),
            title,
            dirty: false,
            editor_state,
            content_hash,
            _suggestion_feedback: suggestion_feedback,
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
        let (editor_state, suggestion_feedback) =
            Self::build_editor_state(Some(content), options, window, cx);
        Self {
            path: None,
            title: untitled_title(number),
            dirty,
            editor_state,
            content_hash: 0,
            _suggestion_feedback: suggestion_feedback,
        }
    }

    /// Builds the editor state for a document: GPUI Kit's text editor, with
    /// jot's spell checker and word suggestions shown as ghost text. The
    /// subscription tells the suggestions when Tab takes one or Escape
    /// closes it.
    fn build_editor_state(
        content: Option<String>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, Subscription) {
        let dictionary = options.shared_vocab.borrow().dictionary();
        let completions = Rc::new(JotCompletionProvider::new(
            options.shared_vocab.clone(),
            options.suggestion_pacing.clone(),
        ));
        let editor_state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .text_editor()
                .soft_wrap(options.word_wrap)
                .line_number(options.line_numbers)
                .keymap(options.keymap)
                .smooth_caret(options.smooth_caret)
                .suggestion_provider(completions.clone())
                .suggestion_options(SuggestionOptions::default().menu(false).inline(true));
            if dictionary.is_loaded() {
                state = state.spell_checker(Rc::new(DocumentSpelling::new(dictionary)));
            }
            if let Some(content) = content {
                state = state.default_value(content);
            }
            state.set_spell_checking(options.spell_check, cx);
            state
        });
        let suggestion_feedback = cx
            .subscribe(&editor_state, move |_, _, event: &SuggestionEvent, cx| {
                completions.suggestion_event(event, cx)
            });
        (editor_state, suggestion_feedback)
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
