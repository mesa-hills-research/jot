use gpui_kit::component::input::{Keymap, Rope, SuggestionOptions, TextareaState};
use gpui_kit::writing::Writing;
use gpui_kit::{App, AppContext, Context, Entity, SharedString, Subscription, Task, Window};
use ropey::extra::esoterica::ropes_are_instances;
use std::path::{Path, PathBuf};

/// How a new document's editor starts out, from the settings.
#[derive(Clone)]
pub struct EditorOptions {
    pub word_wrap: bool,
    pub line_numbers: bool,
    pub spell_check: bool,
    pub keymap: Keymap,
    pub smooth_caret: bool,
    /// The spelling dictionary and the word suggestions' vocabulary and
    /// pacing, shared by every document.
    pub writing: Writing,
}

/// A save of a document, which ends with the message to show if it failed.
pub type Save = futures::future::Shared<Task<Result<(), String>>>;

pub struct Document {
    pub path: Option<PathBuf>,
    pub title: SharedString,
    pub dirty: bool,
    pub editor_state: Entity<TextareaState>,
    /// The text the file has, as opened or last saved. Empty without a file.
    saved_text: Rope,
    /// The text `dirty` was last worked out for, which saves working it out
    /// again until the text changes.
    checked_text: Option<Rope>,
    /// The latest save asked for, and the file it writes. It ends after the
    /// saves asked for before it.
    latest_save: Option<(PathBuf, Save)>,
    /// How many of the saves asked for haven't ended.
    saves_pending: usize,
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
            saved_text: Rope::new(),
            checked_text: None,
            latest_save: None,
            saves_pending: 0,
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
        let (editor_state, suggestion_feedback) =
            Self::build_editor_state(Some(content), options, window, cx);
        let saved_text = editor_state.read(cx).text().clone();
        Self {
            path: Some(path),
            title,
            dirty: false,
            editor_state,
            saved_text,
            checked_text: None,
            latest_save: None,
            saves_pending: 0,
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
            saved_text: Rope::new(),
            checked_text: None,
            latest_save: None,
            saves_pending: 0,
            _suggestion_feedback: suggestion_feedback,
        }
    }

    /// Builds the editor state for a document: GPUI Kit's text editor, with
    /// the writing aids' spell checker and word suggestions shown as ghost
    /// text. The subscription tells the suggestions when Tab takes one or
    /// Escape closes it.
    ///
    /// The spell checker finds nothing misspelled until the dictionary has
    /// loaded, and checks again then.
    fn build_editor_state(
        content: Option<String>,
        options: &EditorOptions,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> (Entity<TextareaState>, Subscription) {
        let suggestions = options.writing.word_suggestions();
        if let Some(content) = &content {
            suggestions.opened_with(content);
        }
        let editor_state = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .text_editor()
                .soft_wrap(options.word_wrap)
                .line_number(options.line_numbers)
                .keymap(options.keymap)
                .smooth_caret(options.smooth_caret)
                .spell_checker(options.writing.spell_checker())
                .suggestion_provider(suggestions.clone())
                .suggestion_options(SuggestionOptions::default().menu(false).inline(true));
            if let Some(content) = content {
                state = state.default_value(content);
            }
            state.set_spell_checking(options.spell_check, cx);
            crate::key_log::record(state, &options.writing, cx)
        });
        let suggestion_feedback = suggestions.follow(&editor_state, cx);
        (editor_state, suggestion_feedback)
    }

    pub fn content(&self, cx: &App) -> String {
        self.editor_state.read(cx).value().to_string()
    }

    /// The text as it is now, which a save writes. Ropes share their text,
    /// so this copies nothing.
    pub fn text(&self, cx: &App) -> Rope {
        self.editor_state.read(cx).text().clone()
    }

    /// Works out whether the text differs from the file's, or for a
    /// document without a file, whether it has any. Until the text changes
    /// this costs nothing, and a change of length shows at once.
    pub fn check_dirty(&mut self, cx: &App) {
        let text = self.editor_state.read(cx).text();
        // True only for copies of one rope that neither has changed since.
        let unchanged = |a: &Rope, b: &Rope| ropes_are_instances(a, b);
        if self
            .checked_text
            .as_ref()
            .is_some_and(|checked| unchanged(text, checked))
        {
            return;
        }
        self.dirty = match self.path {
            None => text.len() > 0,
            Some(_) => {
                text.len() != self.saved_text.len()
                    || !(unchanged(text, &self.saved_text) || *text == self.saved_text)
            }
        };
        self.checked_text = Some(text.clone());
    }

    /// Notes that the file has `text`, and works out again whether the
    /// document has unsaved changes, which it has when it changed since.
    pub fn mark_saved(&mut self, text: Rope, cx: &App) {
        self.saved_text = text;
        self.checked_text = None;
        self.check_dirty(cx);
    }

    pub fn set_path(&mut self, path: PathBuf) {
        self.title = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
            .into();
        self.path = Some(path);
        self.checked_text = None;
    }

    /// The save that ends last of those that haven't ended.
    pub fn pending_save(&self) -> Option<Save> {
        if self.saves_pending == 0 {
            return None;
        }
        self.latest_save.as_ref().map(|(_, save)| save.clone())
    }

    /// The file Save writes. That's the one the latest save asked for goes
    /// to, such as a Save As still running, or else the document's.
    pub fn save_path(&self) -> Option<PathBuf> {
        match &self.latest_save {
            Some((path, _)) if self.saves_pending > 0 => Some(path.clone()),
            _ => self.path.clone(),
        }
    }

    /// Notes a save of the document to `path` that has just started.
    pub fn save_started(&mut self, path: PathBuf, save: Save) {
        self.latest_save = Some((path, save));
        self.saves_pending += 1;
    }

    /// Notes that the oldest save that hadn't ended, of `text` to `path`,
    /// has ended, having written the file or not.
    pub fn save_ended(&mut self, path: &Path, text: Rope, written: bool, cx: &App) {
        self.saves_pending -= 1;
        if written {
            self.set_path(path.to_path_buf());
            self.mark_saved(text, cx);
        }
    }
}

fn untitled_title(number: Option<u32>) -> SharedString {
    match number {
        Some(n) if n > 1 => format!("Untitled {}", n).into(),
        _ => "Untitled".into(),
    }
}
