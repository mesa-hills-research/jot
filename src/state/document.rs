use crate::autocomplete::{JotCompletionProvider, SharedVocabulary};
use crate::spell::{Dictionary, SPELL_CHECK_DEBOUNCE, SpellScanner};
use crate::spell_editor;
use gpui_kit::component::input::{
    EditorState, InputEvent, Position, RopeExt, TextDecorationCollection,
};
use gpui_kit::{AppContext, Context, Entity, SharedString, Subscription, Task, Window};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;

pub struct Document {
    pub path: Option<PathBuf>,
    pub title: SharedString,
    pub dirty: bool,
    pub editor_state: Entity<EditorState>,
    /// The spelling underlines, which follow the text as it is edited.
    pub underlines: TextDecorationCollection,
    content_hash: u64,
    dictionary: Rc<Dictionary>,
    spell_enabled: Rc<Cell<bool>>,
    spell_scanner: SpellScanner,
    _spell_check_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Document {
    #[allow(clippy::too_many_arguments)]
    pub fn new_untitled(
        number: Option<u32>,
        word_wrap: bool,
        line_numbers: bool,
        shared_vocab: Rc<RefCell<SharedVocabulary>>,
        autocomplete_enabled: Rc<Cell<bool>>,
        spell_enabled: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = match number {
            Some(n) if n > 1 => format!("Untitled {}", n).into(),
            _ => "Untitled".into(),
        };

        let dictionary = shared_vocab.borrow().dictionary();
        let editor_state = Self::build_editor_state(
            None,
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            window,
            cx,
        );
        let underlines = spell_editor::create_underlines(&editor_state, cx);
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            path: None,
            title,
            dirty: false,
            editor_state,
            underlines,
            content_hash: 0,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_path(
        path: PathBuf,
        content: String,
        word_wrap: bool,
        line_numbers: bool,
        shared_vocab: Rc<RefCell<SharedVocabulary>>,
        autocomplete_enabled: Rc<Cell<bool>>,
        spell_enabled: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title: SharedString = path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "Unknown".to_string())
            .into();

        let content_hash = Self::hash_content(&content);

        let dictionary = shared_vocab.borrow().dictionary();
        let editor_state = Self::build_editor_state(
            Some(content),
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            window,
            cx,
        );
        let underlines = spell_editor::create_underlines(&editor_state, cx);
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            path: Some(path),
            title,
            dirty: false,
            editor_state,
            underlines,
            content_hash,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_template(
        content: String,
        number: Option<u32>,
        word_wrap: bool,
        line_numbers: bool,
        shared_vocab: Rc<RefCell<SharedVocabulary>>,
        autocomplete_enabled: Rc<Cell<bool>>,
        spell_enabled: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let title = match number {
            Some(n) if n > 1 => format!("Untitled {}", n).into(),
            _ => "Untitled".into(),
        };

        let has_content = !content.is_empty();

        let dictionary = shared_vocab.borrow().dictionary();
        let editor_state = Self::build_editor_state(
            Some(content),
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            window,
            cx,
        );
        let underlines = spell_editor::create_underlines(&editor_state, cx);
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            path: None,
            title,
            dirty: has_content,
            editor_state,
            underlines,
            content_hash: 0,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

    /// Builds the editor state for a document.
    ///
    /// Documents use GPUI Kit's code editor with the plain-text language: no
    /// syntax highlighting, but it is the editor that draws line numbers.
    fn build_editor_state(
        content: Option<String>,
        word_wrap: bool,
        line_numbers: bool,
        shared_vocab: Rc<RefCell<SharedVocabulary>>,
        autocomplete_enabled: Rc<Cell<bool>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<EditorState> {
        cx.new(|cx| {
            let mut state = EditorState::new(window, cx)
                .language("text")
                .soft_wrap(word_wrap)
                .line_number(line_numbers)
                .searchable(true);
            if let Some(content) = content {
                state = state.default_value(content);
            }
            state.lsp_mut().completion_provider = Some(Rc::new(JotCompletionProvider::new(
                shared_vocab,
                autocomplete_enabled,
            )));
            state
        })
    }

    /// Subscribes to editor change events to keep spell checking current.
    ///
    /// The underlines move with the edited text in the meantime, so the
    /// check can wait for a pause in typing.
    fn observe_editor(editor_state: &Entity<EditorState>, cx: &mut Context<Self>) -> Subscription {
        cx.subscribe(editor_state, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.schedule_spell_check(cx);
            }
        })
    }

    /// Schedules a debounced spell check.
    ///
    /// Each call replaces (and thereby cancels) any pending check, so rapid
    /// typing costs one task allocation per keystroke and a single scan runs
    /// after the debounce interval of idle time.
    pub fn schedule_spell_check(&mut self, cx: &mut Context<Self>) {
        if !self.dictionary.is_loaded() {
            return;
        }
        self._spell_check_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SPELL_CHECK_DEBOUNCE).await;
            this.update(cx, |doc, cx| {
                doc.run_spell_check(cx);
            })
            .ok();
        }));
    }

    /// Runs the spell scanner and underlines what it finds.
    fn run_spell_check(&mut self, cx: &mut Context<Self>) {
        if !self.spell_enabled.get() || !self.dictionary.is_loaded() {
            spell_editor::set_underlines(&self.underlines, Vec::new(), cx);
            return;
        }

        // Cloning a rope shares its chunks.
        let (text, cursor) = {
            let state = self.editor_state.read(cx);
            (state.text().clone(), state.cursor_position())
        };
        let found = self
            .spell_scanner
            .scan(&self.dictionary, &text, Some(cursor));
        let ranges: Vec<_> = found
            .iter()
            .map(|issue| {
                text.position_to_offset(&Position::new(issue.line, issue.start_character))
                    ..text.position_to_offset(&Position::new(issue.line, issue.end_character))
            })
            .collect();
        spell_editor::set_underlines(&self.underlines, ranges, cx);
    }

    pub fn dictionary(&self) -> &Rc<Dictionary> {
        &self.dictionary
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
