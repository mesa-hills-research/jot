//! Quick-fix code actions for spelling diagnostics.
//!
//! Registered as a [`CodeActionProvider`] on each document's editor, this
//! surfaces "Change to ..." replacements and "Add to dictionary" through the
//! component's built-in code-action menu (`ctrl-.` or right-click → Show
//! Code Actions). Fix parameters travel through the LSP `CodeAction::data`
//! field.
//!
//! # Re-entrancy
//!
//! The component invokes [`CodeActionProvider::perform_code_action`] from
//! *inside* an update of the editor's `InputState` (see
//! `CodeActionMenu::select_item`, which wraps the call in
//! `state.update_in`). The provider therefore must not update that entity
//! synchronously — doing so double-leases it and panics. This is precisely
//! why the trait returns a [`Task`]: the component detach-awaits it after
//! the triggering update completes, so all entity mutations here are
//! deferred into the returned task. Only the pure in-memory dictionary
//! insertion happens synchronously.

use crate::spell::{Dictionary, SpellIssue};
use crate::state::Document;
use anyhow::Result;
use gpui::{App, Entity, SharedString, Task, WeakEntity, Window};
use gpui_component::input::{CodeActionProvider, InputState, RopeExt};
use lsp_types::{CodeAction, CodeActionKind, Position, TextEdit};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

/// Maximum number of replacement candidates offered in the fix menu.
const MAX_FIX_SUGGESTIONS: usize = 5;

/// The payload carried inside `CodeAction::data`.
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind")]
enum SpellAction {
    Replace {
        line: u32,
        start: u32,
        end: u32,
        text: String,
    },
    AddWord {
        word: String,
    },
}

/// Offers spelling corrections and dictionary additions for the misspelling
/// under the cursor.
///
/// Shares the document's live issue list (kept position-accurate by the
/// document's republish-on-edit remapping) because the component's
/// `DiagnosticSet` does not expose offset queries publicly.
pub struct SpellCodeActionProvider {
    document: WeakEntity<Document>,
    dictionary: Rc<Dictionary>,
    issues: Rc<RefCell<Vec<SpellIssue>>>,
}

impl SpellCodeActionProvider {
    pub fn new(
        document: WeakEntity<Document>,
        dictionary: Rc<Dictionary>,
        issues: Rc<RefCell<Vec<SpellIssue>>>,
    ) -> Self {
        Self {
            document,
            dictionary,
            issues,
        }
    }

    fn issue_at(&self, position: Position) -> Option<SpellIssue> {
        self.issues
            .borrow()
            .iter()
            .find(|issue| {
                issue.line == position.line
                    && issue.start_character <= position.character
                    && position.character <= issue.end_character
            })
            .cloned()
    }
}

impl CodeActionProvider for SpellCodeActionProvider {
    fn id(&self) -> SharedString {
        "jot-spell".into()
    }

    fn code_actions(
        &self,
        state: Entity<InputState>,
        range: Range<usize>,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<Vec<CodeAction>>> {
        let position = state.read(cx).text().offset_to_position(range.start);
        let Some(issue) = self.issue_at(position) else {
            return Task::ready(Ok(Vec::new()));
        };

        let mut suggestions = self.dictionary.suggest(&issue.word, MAX_FIX_SUGGESTIONS);
        if suggestions.is_empty() {
            suggestions = issue.suggestions.clone();
        }

        let mut actions = Vec::new();
        for text in suggestions {
            let data = SpellAction::Replace {
                line: issue.line,
                start: issue.start_character,
                end: issue.end_character,
                text: text.clone(),
            };
            actions.push(CodeAction {
                title: format!("Change to \"{}\"", text),
                kind: Some(CodeActionKind::QUICKFIX),
                data: serde_json::to_value(&data).ok(),
                ..CodeAction::default()
            });
        }
        actions.push(CodeAction {
            title: format!("Add \"{}\" to dictionary", issue.word),
            kind: Some(CodeActionKind::QUICKFIX),
            data: serde_json::to_value(&SpellAction::AddWord {
                word: issue.word.clone(),
            })
            .ok(),
            ..CodeAction::default()
        });

        Task::ready(Ok(actions))
    }

    fn perform_code_action(
        &self,
        state: Entity<InputState>,
        action: CodeAction,
        _push_to_history: bool,
        window: &mut Window,
        cx: &mut App,
    ) -> Task<Result<()>> {
        let Some(data) = action.data else {
            return Task::ready(Ok(()));
        };
        let Ok(op) = serde_json::from_value::<SpellAction>(data) else {
            return Task::ready(Ok(()));
        };

        match op {
            SpellAction::Replace {
                line,
                start,
                end,
                text,
            } => {
                let edit = TextEdit {
                    range: lsp_types::Range {
                        start: Position::new(line, start),
                        end: Position::new(line, end),
                    },
                    new_text: text,
                };
                window.spawn(cx, async move |cx| {
                    cx.update(|window, cx| {
                        state.update(cx, |state, cx| {
                            state.apply_lsp_edits(&vec![edit], window, cx);
                        });
                    })?;
                    Ok(())
                })
            }
            SpellAction::AddWord { word } => {
                self.dictionary.add_word(&word);
                let document = self.document.clone();
                window.spawn(cx, async move |cx| {
                    cx.update(|_, cx| {
                        document
                            .update(cx, |doc, cx| {
                                doc.schedule_spell_check(cx);
                            })
                            .ok();
                    })?;
                    Ok(())
                })
            }
        }
    }
}