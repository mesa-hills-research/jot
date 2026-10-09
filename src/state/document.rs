use crate::autocomplete::{JotCompletionProvider, SharedVocabulary};
use crate::spell::{Dictionary, SpellIssue, SpellScanner, SPELL_CHECK_DEBOUNCE};
use crate::spell_actions::SpellCodeActionProvider;
use gpui_kit::component::highlighter::{Diagnostic, DiagnosticSeverity};
use gpui_kit::component::input::{EditorState, InputEvent, Position, RopeExt};
use gpui_kit::{AppContext, Context, Entity, SharedString, Subscription, Task, WeakEntity, Window};
use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use uuid::Uuid;

/// Severity used for spelling diagnostics. `Error` renders the familiar red
/// wavy underline; switch to `Warning` for the theme's warning color.
const SPELL_DIAGNOSTIC_SEVERITY: DiagnosticSeverity = DiagnosticSeverity::Error;

pub struct Document {
    pub id: Uuid,
    pub path: Option<PathBuf>,
    pub title: SharedString,
    pub dirty: bool,
    pub editor_state: Entity<EditorState>,
    content_hash: u64,
    dictionary: Rc<Dictionary>,
    spell_enabled: Rc<Cell<bool>>,
    spell_scanner: SpellScanner,
    issues: Rc<RefCell<Vec<SpellIssue>>>,
    last_line_count: usize,
    _spell_check_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl Document {
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
        let issues: Rc<RefCell<Vec<SpellIssue>>> = Rc::new(RefCell::new(Vec::new()));
        let editor_state = Self::build_editor_state(
            cx.entity().downgrade(),
            None,
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            dictionary.clone(),
            issues.clone(),
            window,
            cx,
        );
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            id: Uuid::new_v4(),
            path: None,
            title,
            dirty: false,
            editor_state,
            content_hash: 0,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            issues,
            last_line_count: 0,
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

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
        let issues: Rc<RefCell<Vec<SpellIssue>>> = Rc::new(RefCell::new(Vec::new()));
        let editor_state = Self::build_editor_state(
            cx.entity().downgrade(),
            Some(content),
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            dictionary.clone(),
            issues.clone(),
            window,
            cx,
        );
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            id: Uuid::new_v4(),
            path: Some(path),
            title,
            dirty: false,
            editor_state,
            content_hash,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            issues,
            last_line_count: 0,
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

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
        let issues: Rc<RefCell<Vec<SpellIssue>>> = Rc::new(RefCell::new(Vec::new()));
        let editor_state = Self::build_editor_state(
            cx.entity().downgrade(),
            Some(content),
            word_wrap,
            line_numbers,
            shared_vocab,
            autocomplete_enabled,
            dictionary.clone(),
            issues.clone(),
            window,
            cx,
        );
        let subscriptions = vec![Self::observe_editor(&editor_state, cx)];

        let mut doc = Self {
            id: Uuid::new_v4(),
            path: None,
            title,
            dirty: has_content,
            editor_state,
            content_hash: 0,
            dictionary,
            spell_enabled,
            spell_scanner: SpellScanner::new(),
            issues,
            last_line_count: 0,
            _spell_check_task: None,
            _subscriptions: subscriptions,
        };
        doc.schedule_spell_check(cx);
        doc
    }

    /// Builds the editor state for a document.
    ///
    /// Documents use GPUI Kit's code editor with the plain-text language: no
    /// syntax highlighting, but it is the editor that draws line numbers and
    /// the diagnostics that carry the spelling underlines.
    fn build_editor_state(
        document: WeakEntity<Self>,
        content: Option<String>,
        word_wrap: bool,
        line_numbers: bool,
        shared_vocab: Rc<RefCell<SharedVocabulary>>,
        autocomplete_enabled: Rc<Cell<bool>>,
        dictionary: Rc<Dictionary>,
        issues: Rc<RefCell<Vec<SpellIssue>>>,
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
                .lsp_mut()
                .code_action_providers
                .push(Rc::new(SpellCodeActionProvider::new(
                    document, dictionary, issues,
                )));
            state
        })
    }

    /// Subscribes to editor change events to keep spell checking current.
    ///
    /// Existing squiggles are republished immediately (remapped for the
    /// edit) so they do not blink out during the debounce window, and a full
    /// re-scan is scheduled.
    fn observe_editor(
        editor_state: &Entity<EditorState>,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe(editor_state, |this, _, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                this.republish_existing_issues(cx);
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

    /// Re-publishes the previous scan's issues right after an edit, since
    /// the component clears its diagnostic set on every text change.
    ///
    /// Issues on the cursor's line are dropped (their columns are stale),
    /// lines above the edit are kept verbatim, and lines below are shifted
    /// by the line-count delta. Multi-line changes (paste, replace-all)
    /// cannot be remapped reliably, so they fall through to the debounced
    /// scan. Cost is O(issues) per keystroke.
    fn republish_existing_issues(&mut self, cx: &mut Context<Self>) {
        if !self.spell_enabled.get() || !self.dictionary.is_loaded() {
            return;
        }
        if self.issues.borrow().is_empty() {
            return;
        }

        let editor_state = self.editor_state.clone();
        let issues = self.issues.clone();
        let last_line_count = &mut self.last_line_count;

        editor_state.update(cx, |state, cx| {
            let new_line_count = state.text().lines_len();
            let delta = new_line_count as i64 - *last_line_count as i64;

            if delta.abs() > 1 {
                issues.borrow_mut().clear();
                *last_line_count = new_line_count;
                return;
            }

            let cursor_line = state.cursor_position().line as i64;
            let edit_line = if delta > 0 { cursor_line - delta } else { cursor_line };

            let mut kept: Vec<SpellIssue> = Vec::new();
            for mut issue in issues.borrow_mut().drain(..) {
                let line = issue.line as i64;
                if line < edit_line {
                    kept.push(issue);
                } else if line <= edit_line - delta.min(0) {
                    continue;
                } else {
                    let shifted = line + delta;
                    if shifted >= 0 && (shifted as usize) < new_line_count {
                        issue.line = shifted as u32;
                        kept.push(issue);
                    }
                }
            }

            if let Some(diagnostics) = state.diagnostics_mut() {
                diagnostics.clear();
                for issue in &kept {
                    diagnostics.push(Self::diagnostic_for(issue));
                }
                cx.notify();
            }

            *issues.borrow_mut() = kept;
            *last_line_count = new_line_count;
        });
    }

    /// Runs the spell scanner and publishes the results as diagnostics.
    fn run_spell_check(&mut self, cx: &mut Context<Self>) {
        let editor_state = self.editor_state.clone();
        let enabled = self.spell_enabled.get() && self.dictionary.is_loaded();
        let dictionary = self.dictionary.clone();
        let issues = self.issues.clone();
        let scanner = &mut self.spell_scanner;
        let last_line_count = &mut self.last_line_count;

        editor_state.update(cx, |state, cx| {
            if !enabled {
                issues.borrow_mut().clear();
                if let Some(diagnostics) = state.diagnostics_mut() {
                    if !diagnostics.is_empty() {
                        diagnostics.clear();
                        cx.notify();
                    }
                }
                return;
            }

            let cursor = state.cursor_position();
            let found = scanner.scan(&dictionary, state.text(), Some(cursor));

            if let Some(diagnostics) = state.diagnostics_mut() {
                diagnostics.clear();
                for issue in &found {
                    diagnostics.push(Self::diagnostic_for(issue));
                }
                cx.notify();
            }

            *last_line_count = state.text().lines_len();
            *issues.borrow_mut() = found;
        });
    }

    /// Converts a spell issue into a component diagnostic, embedding any
    /// correction suggestions into the hover message.
    fn diagnostic_for(issue: &SpellIssue) -> Diagnostic {
        let start = Position::new(issue.line, issue.start_character);
        let end = Position::new(issue.line, issue.end_character);

        let mut message = format!("Unknown word \"{}\"", issue.word);
        if !issue.suggestions.is_empty() {
            message.push_str("\n\nDid you mean: ");
            message.push_str(&issue.suggestions.join(", "));
            message.push('?');
        }

        Diagnostic::new(start..end, message)
            .with_severity(SPELL_DIAGNOSTIC_SEVERITY)
            .with_source("spell")
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