//! How the spell checker meets the editor.
//!
//! Misspellings are drawn as wavy text decorations, which move with the text
//! as it is edited. Right-clicking one offers the fixes above the usual Cut,
//! Copy, Paste and Select All: up to five replacements, Add to Dictionary and
//! Ignore.
//!
//! This is interim glue on GPUI Kit's code editor, kept in one place for the
//! switch to a text editor mode with spell checking of its own.

use crate::state::{AppState, Document};
use gpui_kit::component::ActiveTheme;
use gpui_kit::component::input::{
    Copy, Cut, EditorState, Paste, SelectAll, TextDecoration, TextDecorationCollection,
};
use gpui_kit::component::native_menu::NativeMenu;
use gpui_kit::*;
use std::ops::Range;

/// Most replacements the right-click menu offers.
const MAX_SUGGESTIONS: usize = 5;

/// Replaces the misspelling at `range` with `replacement`, as one undo step.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = jot, no_json)]
pub struct ReplaceMisspelling {
    /// The misspelling's byte range when the menu opened.
    pub range: Range<usize>,
    /// The misspelling, so a fix is skipped if the text changed meanwhile.
    pub word: SharedString,
    pub replacement: SharedString,
}

/// Adds `word` to the user dictionary.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = jot, no_json)]
pub struct AddToDictionary {
    pub word: SharedString,
}

/// Accepts `word` until jot closes.
#[derive(Clone, PartialEq, Debug, Action)]
#[action(namespace = jot, no_json)]
pub struct IgnoreWord {
    pub word: SharedString,
}

/// Creates the collection that holds a document's underlines.
pub fn create_underlines(editor: &Entity<EditorState>, cx: &mut App) -> TextDecorationCollection {
    editor.update(cx, |state, cx| {
        state.create_decorations_collection(Vec::new(), cx)
    })
}

/// Underlines `ranges`, byte ranges into the document, in place of the
/// previous underlines.
pub fn set_underlines(
    underlines: &TextDecorationCollection,
    ranges: impl IntoIterator<Item = Range<usize>>,
    cx: &mut App,
) {
    let style = HighlightStyle {
        underline: Some(UnderlineStyle {
            color: Some(cx.theme().highlight_theme.style.status.error(cx)),
            thickness: px(1.),
            wavy: true,
        }),
        ..Default::default()
    };
    let decorations = ranges
        .into_iter()
        .map(|range| TextDecoration::new(range, style))
        .collect();
    underlines.set(decorations, cx);
}

/// The underlined word at `offset`, if it is still misspelled.
fn misspelling_at(doc: &Document, offset: usize, cx: &App) -> Option<(Range<usize>, String)> {
    let text = doc.editor_state.read(cx).text();
    let range = doc
        .underlines
        .get_ranges(cx)
        .into_iter()
        .find(|range| range.start <= offset && offset <= range.end)?;
    let word = text.try_slice(range.clone()).ok()?.to_string();
    (!doc.dictionary().is_correct(&word)).then_some((range, word))
}

/// The editor's right-click menu, for `Editor::context_menu`.
///
/// The editor calls it while it is still updating its own state, which can't
/// be read then, so it hands the editor an empty menu (shown as nothing) and
/// shows the real one right after, at the pointer.
pub fn context_menu(
    doc: Entity<Document>,
) -> impl Fn(NativeMenu, &mut Window, &mut App) -> NativeMenu + 'static {
    move |_, window, cx| {
        let doc = doc.clone();
        let position = window.mouse_position();
        window.defer(cx, move |window, cx| {
            build_context_menu(&doc, cx).show(position, window, cx);
        });
        NativeMenu::new()
    }
}

/// Fixes for the underlined word under the click, then the standard items.
///
/// The editor has moved the caret to the clicked character by now, unless the
/// click landed in the selection.
fn build_context_menu(doc: &Entity<Document>, cx: &App) -> NativeMenu {
    let doc = doc.read(cx);
    let state = doc.editor_state.read(cx);
    let mut menu = NativeMenu::new();

    if let Some((range, word)) = misspelling_at(doc, state.cursor(), cx) {
        let word: SharedString = word.into();
        let suggestions = doc.dictionary().suggest(&word, MAX_SUGGESTIONS);
        if suggestions.is_empty() {
            menu = menu.menu_with_disabled("No Suggestions", true, Box::new(NoAction));
        }
        for replacement in suggestions {
            menu = menu.menu(
                replacement.clone(),
                Box::new(ReplaceMisspelling {
                    range: range.clone(),
                    word: word.clone(),
                    replacement: replacement.into(),
                }),
            );
        }
        menu = menu
            .separator()
            .menu(
                "Add to Dictionary",
                Box::new(AddToDictionary { word: word.clone() }),
            )
            .menu("Ignore", Box::new(IgnoreWord { word }))
            .separator();
    }

    // A custom menu replaces the editor's own, so the standard items go here.
    let capabilities = state.context_menu_capabilities();
    let editable = capabilities.is_editable();
    menu.menu_with_disabled(
        "Cut",
        !(editable && capabilities.is_copyable()),
        Box::new(Cut),
    )
    .menu_with_disabled("Copy", !capabilities.is_copyable(), Box::new(Copy))
    .menu_with_disabled("Paste", !editable, Box::new(Paste))
    .separator()
    .menu("Select All", Box::new(SelectAll))
}

/// Handles the menu's spelling actions. They go to the focused editor and
/// bubble up to `element`.
pub fn on_spelling_actions(element: Div, app_state: &Entity<AppState>) -> Div {
    element
        .on_action({
            let app_state = app_state.clone();
            move |action: &ReplaceMisspelling, window, cx| {
                let Some(doc) = app_state.read(cx).active_document().cloned() else {
                    return;
                };
                let editor = doc.read(cx).editor_state.clone();
                editor.update(cx, |state, cx| {
                    let unchanged = state
                        .text()
                        .try_slice(action.range.clone())
                        .is_ok_and(|current| current == action.word.as_ref());
                    if unchanged {
                        state.set_selected_range(action.range.clone(), cx);
                        state.replace(action.replacement.clone(), window, cx);
                    }
                });
            }
        })
        .on_action({
            let app_state = app_state.clone();
            move |action: &AddToDictionary, _, cx| {
                app_state.update(cx, |state, cx| {
                    state.dictionary().add_word(&action.word);
                    state.recheck_spelling(cx);
                });
            }
        })
        .on_action({
            let app_state = app_state.clone();
            move |action: &IgnoreWord, _, cx| {
                app_state.update(cx, |state, cx| {
                    state.dictionary().ignore_word(&action.word);
                    state.recheck_spelling(cx);
                });
            }
        })
}
