//! Suggestions in jot's text editor, typed a key at a time on GPUI's fake
//! clock.

use super::SharedVocabulary;
use crate::state::{Document, EditorOptions};
use gpui_kit::component::input::{Keymap, TextEditor, TextareaState};
use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    TestAppContext, VisualTestContext, Window, div, px, size,
};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// A vocabulary that knows `hello` well, `help` and `helium` less, and that
/// `world` follows `hello`.
fn vocabulary() -> Rc<RefCell<SharedVocabulary>> {
    let mut vocabulary = SharedVocabulary::new();
    let words = [
        ("hello", 20),
        ("help", 5),
        ("helium", 1),
        ("world", 10),
        ("wonder", 3),
    ];
    for (word, count) in words {
        for _ in 0..count {
            vocabulary.learn_word(word);
        }
    }
    for _ in 0..4 {
        vocabulary.learn_bigram("hello", "world");
    }
    Rc::new(RefCell::new(vocabulary))
}

struct Root {
    editor: Entity<TextareaState>,
    _document: Entity<Document>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(TextEditor::new(&self.editor))
    }
}

/// A jot document in a window, with the focus in its editor.
struct Jot {
    cx: VisualTestContext,
    editor: Entity<TextareaState>,
}

impl Jot {
    fn new(cx: &mut TestAppContext, enabled: bool) -> Self {
        cx.update(gpui_kit::init);
        let options = EditorOptions {
            word_wrap: false,
            line_numbers: false,
            spell_check: false,
            keymap: Keymap::Cua,
            smooth_caret: false,
            shared_vocab: vocabulary(),
            autocomplete_enabled: Rc::new(Cell::new(enabled)),
        };
        let window = cx.open_window(size(px(600.), px(300.)), move |window, cx| {
            let document = cx.new(|cx| Document::new_untitled(None, &options, window, cx));
            let editor = document.read(cx).editor_state.clone();
            Root {
                editor,
                _document: document,
            }
        });
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let editor = window
            .read_with(&cx, |root, _| root.editor.clone())
            .unwrap();
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| state.focus(window, cx));
            window.draw(cx).clear(cx);
        });
        Self { cx, editor }
    }

    /// The ghost text after the caret.
    fn ghost(&self) -> Option<String> {
        self.editor.read_with(&self.cx, |state, _| {
            state
                .suggestions()
                .first()
                .map(|suggestion| suggestion.text().to_string())
        })
    }

    fn value(&self) -> String {
        self.editor
            .read_with(&self.cx, |state, _| state.value().to_string())
    }

    /// Types `text` a key at a time, and returns the ghost text after each.
    fn ghosts_while_typing(&mut self, text: &str) -> Vec<Option<String>> {
        text.chars()
            .map(|key| {
                self.cx.simulate_input(&key.to_string());
                self.ghost()
            })
            .collect()
    }
}

fn ghosts(expected: &[Option<&str>]) -> Vec<Option<String>> {
    expected.iter().map(|ghost| ghost.map(String::from)).collect()
}

/// Every keystroke brings the model's suggestion up at once, again after
/// Escape and after typing past it.
#[gpui_kit::test]
fn suggestions_follow_every_keystroke(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, true);

    assert_eq!(
        jot.ghosts_while_typing("hello world he"),
        ghosts(&[
            None,
            Some("llo world"),
            Some("lo world"),
            Some("o world"),
            None,
            Some("world"),
            Some("orld"),
            Some("rld"),
            Some("ld"),
            Some("d"),
            None,
            None,
            None,
            Some("llo world"),
        ])
    );
    jot.cx.simulate_keystrokes("escape");
    assert_eq!(jot.ghost(), None);
    assert_eq!(
        jot.ghosts_while_typing("lp he"),
        ghosts(&[Some("lo world"), None, None, None, Some("llo world")])
    );
    jot.cx.simulate_keystrokes("tab");
    assert_eq!(jot.value(), "hello world help hello world");
    assert_eq!(jot.ghost(), None);
}

/// With suggestions turned off, typing offers none.
#[gpui_kit::test]
fn turned_off_offers_nothing(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, false);
    assert!(
        jot.ghosts_while_typing("hello world he")
            .iter()
            .all(Option::is_none)
    );
}
