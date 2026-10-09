//! Suggestions in jot's text editor, typed a key at a time on GPUI's fake
//! clock.

use super::pacing::{
    ESCAPE_WEIGHT, IGNORED_EXTRA_PAUSE, MAX_QUIET_PAUSE, QUIET_PAUSE, SUPPRESSION,
};
use super::{AutocompleteMode, SharedVocabulary, SuggestionPacing};
use crate::state::{Document, EditorOptions};
use gpui_kit::component::input::{Keymap, TextEditor, TextareaState};
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, TestAppContext, VisualTestContext, Window, div, px, size,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

/// A vocabulary that knows `hello` well, `help` and `helium` less, and that
/// `world` follows `hello`.
fn vocabulary() -> SharedVocabulary {
    let mut vocabulary = words(&[
        ("hello", 20),
        ("help", 5),
        ("helium", 1),
        ("world", 10),
        ("wonder", 3),
    ]);
    for _ in 0..4 {
        vocabulary.learn_bigram("hello", "world");
    }
    vocabulary
}

fn words(words: &[(&str, u32)]) -> SharedVocabulary {
    let mut vocabulary = SharedVocabulary::new();
    for &(word, count) in words {
        for _ in 0..count {
            vocabulary.learn_word(word);
        }
    }
    vocabulary
}

fn ms(ms: u64) -> Duration {
    Duration::from_millis(ms)
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
    pacing: Rc<SuggestionPacing>,
    vocabulary: Rc<RefCell<SharedVocabulary>>,
}

impl Jot {
    fn new(cx: &mut TestAppContext, mode: AutocompleteMode) -> Self {
        Self::with_vocabulary(cx, mode, vocabulary())
    }

    fn with_vocabulary(
        cx: &mut TestAppContext,
        mode: AutocompleteMode,
        vocabulary: SharedVocabulary,
    ) -> Self {
        Self::open(cx, mode, Rc::new(RefCell::new(vocabulary)), None)
    }

    /// A document with `content`, or a new one, learning into `vocabulary`.
    fn open(
        cx: &mut TestAppContext,
        mode: AutocompleteMode,
        vocabulary: Rc<RefCell<SharedVocabulary>>,
        content: Option<&str>,
    ) -> Self {
        cx.update(gpui_kit::init);
        let pacing = Rc::new(SuggestionPacing::new(mode));
        let options = EditorOptions {
            word_wrap: false,
            line_numbers: false,
            spell_check: false,
            keymap: Keymap::Cua,
            smooth_caret: false,
            shared_vocab: vocabulary.clone(),
            suggestion_pacing: pacing.clone(),
        };
        let content = content.map(str::to_string);
        let window = cx.open_window(size(px(600.), px(300.)), move |window, cx| {
            let document = cx.new(|cx| match content {
                Some(content) => Document::from_template(content, None, &options, window, cx),
                None => Document::new_untitled(None, &options, window, cx),
            });
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
        Self {
            cx,
            editor,
            pacing,
            vocabulary,
        }
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

    /// Types `text` a key at a time, with no time between the keys.
    fn type_text(&mut self, text: &str) {
        self.cx.simulate_input(text);
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

    fn wait(&mut self, duration: Duration) {
        self.cx.executor().advance_clock(duration);
        self.cx.run_until_parked();
    }

    /// Waits until a suggestion shows, and returns how long that took, or
    /// `None` when none does within twice the longest pause.
    fn pause_before_a_suggestion(&mut self) -> Option<Duration> {
        let step = ms(50);
        let mut waited = Duration::ZERO;
        while self.ghost().is_none() {
            if waited > 2 * MAX_QUIET_PAUSE {
                return None;
            }
            self.wait(step);
            waited += step;
        }
        Some(waited)
    }

    /// Types `text`, then waits for a suggestion as long as Quiet ever waits.
    fn type_and_wait(&mut self, text: &str) -> Option<String> {
        self.type_text(text);
        self.wait(MAX_QUIET_PAUSE);
        self.ghost()
    }
}

fn ghosts(expected: &[Option<&str>]) -> Vec<Option<String>> {
    expected
        .iter()
        .map(|ghost| ghost.map(String::from))
        .collect()
}

/// In Eager, every keystroke brings the model's suggestion up at once, again
/// after Escape and after typing past it.
#[gpui_kit::test]
fn eager_suggests_after_every_keystroke(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Eager);

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

/// Off offers nothing, however long typing pauses.
#[gpui_kit::test]
fn off_offers_nothing(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Off);
    assert!(
        jot.ghosts_while_typing("hello world he")
            .iter()
            .all(Option::is_none)
    );
    assert_eq!(jot.pause_before_a_suggestion(), None);
}

/// Quiet shows nothing while keys come 60 ms apart, and the suggestion once
/// typing has paused for the pause.
#[gpui_kit::test]
fn quiet_waits_for_typing_to_pause(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    for key in "hello wor".chars() {
        jot.type_text(&key.to_string());
        assert_eq!(jot.ghost(), None);
        jot.wait(ms(60));
        assert_eq!(jot.ghost(), None);
    }
    jot.wait(QUIET_PAUSE - ms(61));
    assert_eq!(jot.ghost(), None);
    jot.wait(ms(1));
    assert_eq!(jot.ghost().as_deref(), Some("ld"));
}

/// A suggestion on screen stays there, without a pause, while the user types
/// the letters it has next.
#[gpui_kit::test]
fn quiet_keeps_a_suggestion_typed_along(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    assert_eq!(jot.type_and_wait("hel").as_deref(), Some("lo world"));
    assert_eq!(jot.ghosts_while_typing("l"), ghosts(&[Some("o world")]));
    jot.cx.simulate_keystrokes("tab");
    assert_eq!(jot.value(), "hello world");
}

/// A word typed past is not offered again after the same prefix, or a longer
/// one, until the suppression time is over. A shorter prefix still offers it.
#[gpui_kit::test]
fn a_word_typed_past_comes_back_after_the_suppression_time(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    assert_eq!(jot.type_and_wait("hel").as_deref(), Some("lo world"));
    jot.type_text("p");

    assert_eq!(jot.type_and_wait(" hel"), None);
    assert_eq!(jot.type_and_wait("l"), None);
    assert_eq!(jot.type_and_wait(" he").as_deref(), Some("llo world"));
    jot.cx.simulate_keystrokes("tab");

    jot.wait(SUPPRESSION / 2);
    assert_eq!(jot.type_and_wait(" hel"), None);
    jot.wait(SUPPRESSION / 2);
    assert_eq!(jot.type_and_wait(" hel").as_deref(), Some("lo world"));
}

/// Typing the rest of the word, Enter and Escape all leave the suggestion
/// behind: each suppresses it and lengthens the next pause, Escape by more.
#[gpui_kit::test]
fn finishing_the_word_enter_and_escape_ignore_the_suggestion(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    assert_eq!(jot.type_and_wait("hel").as_deref(), Some("lo world"));
    assert_eq!(
        jot.ghosts_while_typing("lo"),
        ghosts(&[Some("o world"), None])
    );
    assert_eq!(jot.type_and_wait(" hel"), None, "finished without Tab");

    jot.type_text(" wo");
    assert_eq!(
        jot.pause_before_a_suggestion(),
        Some(QUIET_PAUSE + IGNORED_EXTRA_PAUSE)
    );
    jot.cx.simulate_keystrokes("enter");
    assert_eq!(jot.type_and_wait("wo"), None, "left with Enter");

    jot.type_text(" wond");
    assert_eq!(
        jot.pause_before_a_suggestion(),
        Some(QUIET_PAUSE + IGNORED_EXTRA_PAUSE * 2)
    );
    assert_eq!(jot.ghost().as_deref(), Some("er"));
    jot.cx.simulate_keystrokes("escape");
    assert_eq!(jot.type_and_wait(" wond"), None, "refused with Escape");

    jot.type_text(" he");
    assert_eq!(
        jot.pause_before_a_suggestion(),
        Some(QUIET_PAUSE + IGNORED_EXTRA_PAUSE * (2 + ESCAPE_WEIGHT))
    );
}

/// Deleting, or moving the caret and typing elsewhere, leaves a suggestion
/// without counting it as ignored.
#[gpui_kit::test]
fn deleting_or_typing_elsewhere_ignores_nothing(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    assert_eq!(jot.type_and_wait("hel").as_deref(), Some("lo world"));
    jot.cx.simulate_keystrokes("backspace");
    jot.type_text("l");
    assert_eq!(jot.pause_before_a_suggestion(), Some(QUIET_PAUSE));
    assert_eq!(jot.ghost().as_deref(), Some("lo world"));

    jot.cx.simulate_keystrokes("left");
    jot.type_text("x");
    jot.cx.simulate_keystrokes("end");
    assert_eq!(jot.value(), "hexl");
    jot.type_text(" hel");
    assert_eq!(jot.pause_before_a_suggestion(), Some(QUIET_PAUSE));
    assert_eq!(jot.ghost().as_deref(), Some("lo world"));
}

/// Each suggestion ignored in a row lengthens the pause, up to the cap, and
/// accepting one brings the plain pause back.
#[gpui_kit::test]
fn ignored_suggestions_lengthen_the_pause_until_one_is_accepted(cx: &mut TestAppContext) {
    let names = [
        "alpha", "bravo", "charlie", "delta", "echoes", "foxtrot", "golfer", "hotel", "india",
    ];
    let vocabulary = words(&names.map(|name| (name, 10)));
    let mut jot = Jot::with_vocabulary(cx, AutocompleteMode::Quiet, vocabulary);

    let mut pauses = Vec::new();
    for name in &names[..7] {
        jot.type_text(&name[..2]);
        pauses.push(jot.pause_before_a_suggestion().unwrap());
        jot.type_text(" ");
    }
    assert_eq!(
        pauses,
        [
            ms(300),
            ms(450),
            ms(600),
            ms(750),
            ms(900),
            ms(1000),
            ms(1000)
        ]
    );
    assert_eq!(MAX_QUIET_PAUSE, ms(1000));

    jot.type_text("ho");
    assert_eq!(jot.pause_before_a_suggestion(), Some(MAX_QUIET_PAUSE));
    jot.cx.simulate_keystrokes("tab");
    assert!(jot.value().ends_with(" hotel"), "{}", jot.value());
    jot.type_text(" in");
    assert_eq!(jot.pause_before_a_suggestion(), Some(QUIET_PAUSE));
}

/// A new mode applies from the next keystroke.
#[gpui_kit::test]
fn a_new_mode_applies_at_once(cx: &mut TestAppContext) {
    let mut jot = Jot::new(cx, AutocompleteMode::Quiet);
    assert_eq!(jot.ghosts_while_typing("he"), ghosts(&[None, None]));

    jot.pacing.set_mode(AutocompleteMode::Eager);
    assert_eq!(jot.ghosts_while_typing("l"), ghosts(&[Some("lo world")]));

    jot.pacing.set_mode(AutocompleteMode::Off);
    assert_eq!(jot.type_and_wait(" he"), None);

    jot.pacing.set_mode(AutocompleteMode::Quiet);
    assert_eq!(jot.ghosts_while_typing(" he"), ghosts(&[None, None, None]));
    assert_eq!(jot.pause_before_a_suggestion(), Some(QUIET_PAUSE));
}

/// The words a jot document's vocabulary has learned, with their counts.
fn learned(jot: &Jot) -> Vec<(String, u32)> {
    let vocabulary = jot.vocabulary.borrow();
    let mut words: Vec<(String, u32)> = vocabulary
        .store
        .words
        .iter()
        .map(|(word, &count)| (word.clone(), count))
        .collect();
    words.sort();
    words
}

fn counts(words: &[(&str, u32)]) -> Vec<(String, u32)> {
    let mut words: Vec<(String, u32)> = words
        .iter()
        .map(|&(word, count)| (word.to_string(), count))
        .collect();
    words.sort();
    words
}

/// Runs `script` on an empty vocabulary in Eager and then in Quiet, checks
/// that both learned the same words, and returns them.
fn learned_in_both_modes(cx: &mut TestAppContext, script: impl Fn(&mut Jot)) -> Vec<(String, u32)> {
    let mut results = Vec::new();
    for mode in [AutocompleteMode::Eager, AutocompleteMode::Quiet] {
        let mut jot = Jot::with_vocabulary(cx, mode, SharedVocabulary::new());
        script(&mut jot);
        results.push(learned(&jot));
    }
    assert_eq!(results[1], results[0], "Quiet learns what Eager learns");
    results.remove(0)
}

fn paste(jot: &mut Jot, text: &str) {
    let item = ClipboardItem::new_string(text.to_string());
    jot.cx.update(|_, cx| cx.write_to_clipboard(item));
    jot.cx.simulate_keystrokes("ctrl-v");
}

/// Each sentence is learned when its terminator is typed, though Quiet asks
/// for suggestions only once typing pauses.
#[gpui_kit::test]
fn typing_sentences_learns_them(cx: &mut TestAppContext) {
    let learned = learned_in_both_modes(cx, |jot| {
        jot.type_text("Ferns grow slowly. Mosses grow faster. ");
    });
    assert_eq!(
        learned,
        counts(&[
            ("Ferns", 1),
            ("Mosses", 1),
            ("faster", 1),
            ("grow", 2),
            ("slowly", 1)
        ])
    );
}

/// A paste teaches nothing, neither when the user types after it nor when
/// they finish a pasted sentence. What they type below it is learned.
#[gpui_kit::test]
fn a_paste_learns_nothing(cx: &mut TestAppContext) {
    let learned = learned_in_both_modes(cx, |jot| {
        paste(jot, "Ferns grow slowly.\nMosses grow faster. Lichens wait");
        assert_eq!(
            jot.value(),
            "Ferns grow slowly.\nMosses grow faster. Lichens wait"
        );
        assert_eq!(learned(jot), []);
        jot.type_text(" patiently. Then");
    });
    assert_eq!(learned, []);

    let learned = learned_in_both_modes(cx, |jot| {
        paste(jot, "Lichens wait patiently");
        jot.type_text(".");
        paste(jot, " Ferns grow slowly.\n");
        jot.type_text("Mosses grow faster. Then");
    });
    assert_eq!(
        learned,
        counts(&[("Mosses", 1), ("faster", 1), ("grow", 1)])
    );

    // Typing inside pasted text doesn't make it the user's.
    let learned = learned_in_both_modes(cx, |jot| {
        paste(jot, "Alpha beta gamma. Delta epsilon zeta.");
        jot.cx.simulate_keystrokes("left");
        jot.type_text(" eta");
        jot.cx.simulate_keystrokes("end");
        jot.type_text(" Theta");
    });
    assert_eq!(learned, []);
}

/// A line is learned once the user moves on from it, after a blank line
/// too, as is the last item of a list before one.
#[gpui_kit::test]
fn a_line_before_a_blank_line_is_learned(cx: &mut TestAppContext) {
    let learned = learned_in_both_modes(cx, |jot| {
        jot.type_text("Let me know what you think");
        jot.cx.simulate_keystrokes("enter enter");
        jot.type_text("Sounds");
    });
    assert_eq!(
        learned,
        counts(&[
            ("Let", 1),
            ("know", 1),
            ("me", 1),
            ("think", 1),
            ("what", 1),
            ("you", 1)
        ])
    );

    let learned = learned_in_both_modes(cx, |jot| {
        jot.type_text("- buy milk");
        jot.cx.simulate_keystrokes("enter");
        jot.type_text("- call the bank");
        jot.cx.simulate_keystrokes("enter enter");
        jot.type_text("Next");
    });
    assert_eq!(
        learned,
        counts(&[
            ("bank", 1),
            ("buy", 1),
            ("call", 1),
            ("milk", 1),
            ("the", 1)
        ])
    );
}

/// Undo and redo teach nothing: a sentence typed once is learned once.
#[gpui_kit::test]
fn undo_and_redo_learn_nothing(cx: &mut TestAppContext) {
    let learned = learned_in_both_modes(cx, |jot| {
        jot.type_text("Ferns grow slowly.");
        let once = learned(jot);
        assert_eq!(once.len(), 3);
        jot.cx.simulate_keystrokes("ctrl-z");
        assert_ne!(jot.value(), "Ferns grow slowly.");
        jot.cx.simulate_keystrokes("ctrl-shift-z");
        assert_eq!(jot.value(), "Ferns grow slowly.");
        assert_eq!(learned(jot), once);
        jot.type_text(" Mosses grow.");
        assert_eq!(jot.value(), "Ferns grow slowly. Mosses grow.");
    });
    assert_eq!(
        learned,
        counts(&[("Ferns", 1), ("Mosses", 1), ("grow", 2), ("slowly", 1)])
    );

    // A sentence typed with suggestions off isn't learned from its redo.
    let learned = learned_in_both_modes(cx, |jot| {
        let mode = jot.pacing.mode();
        jot.pacing.set_mode(AutocompleteMode::Off);
        jot.type_text("Ferns grow slowly.");
        jot.pacing.set_mode(mode);
        jot.cx.simulate_keystrokes("ctrl-z");
        jot.cx.simulate_keystrokes("ctrl-shift-z");
        assert_eq!(jot.value(), "Ferns grow slowly.");
    });
    assert_eq!(learned, []);
}

/// How many times the vocabulary has learned `word`.
fn count(jot: &Jot, word: &str) -> u32 {
    learned(jot)
        .into_iter()
        .find(|(learned, _)| learned == word)
        .map_or(0, |(_, count)| count)
}

/// A sentence is learned once, whatever happens around it: an edit above it
/// doesn't make it new again, and reopening the document doesn't either. A
/// second copy the user types is learned.
#[gpui_kit::test]
fn a_sentence_is_learned_once(cx: &mut TestAppContext) {
    let learned = learned_in_both_modes(cx, |jot| {
        jot.type_text("First line here.\nLet me know what you think. ");
        jot.cx.simulate_keystrokes("ctrl-home");
        jot.type_text("A");
        jot.cx.simulate_keystrokes("ctrl-end");
        jot.type_text("So");
        assert_eq!(count(jot, "know"), 1);
    });
    assert_eq!(learned.len(), 9, "{learned:?}");

    for mode in [AutocompleteMode::Eager, AutocompleteMode::Quiet] {
        let vocabulary = Rc::new(RefCell::new(SharedVocabulary::new()));
        let mut jot = Jot::open(cx, mode, vocabulary.clone(), None);
        jot.type_text("Let me know what you think. ");
        assert_eq!(count(&jot, "know"), 1);

        let content = jot.value();
        let mut reopened = Jot::open(cx, mode, vocabulary.clone(), Some(&content));
        reopened.cx.simulate_keystrokes("ctrl-end");
        reopened.type_text("So");
        assert_eq!(reopened.value(), "Let me know what you think. So");
        assert_eq!((count(&reopened, "know"), count(&reopened, "So")), (1, 0));
        reopened.type_text(". Let me know what you think. Then");
        assert_eq!((count(&reopened, "know"), count(&reopened, "So")), (2, 1));
    }
}

/// A contraction is one word, with either apostrophe: "don't" is learned
/// whole, completed whole, and predicts what follows it.
#[gpui_kit::test]
fn contractions_are_one_word(cx: &mut TestAppContext) {
    let mut jot = Jot::with_vocabulary(cx, AutocompleteMode::Eager, SharedVocabulary::new());
    for _ in 0..6 {
        jot.type_text("I don't know what to do. We’ll see. ");
    }
    assert_eq!((count(&jot, "don't"), count(&jot, "We’ll")), (6, 6));
    assert_eq!((count(&jot, "don"), count(&jot, "ll")), (0, 0));

    jot.type_text("I don't k");
    assert_eq!(jot.ghost().as_deref(), Some("now what to"));
    jot.type_text("now. I d");
    assert_eq!(jot.ghost().as_deref(), Some("on't know what"));
}

/// A note with a colon in it reads as prose, while code with colons in it
/// stays code.
#[test]
fn a_colon_before_a_space_reads_as_prose() {
    use super::{TextContext, classify_context};
    for line in [
        "Pick up groceries: eggs, milk, bread",
        "Call Mom re: Thanksgiving plans",
        "Notes 10/12: ask about PTO, send the invoice",
    ] {
        assert!(
            classify_context(line, line.len()) == TextContext::Prose,
            "{line}"
        );
    }
    for code in [
        "fn shift(range: Range<usize>, new_len: usize) -> usize {\n    let end: usize = range.end;",
        "def area(width: int, height: int) -> int:\n    return {\"width\": width}",
    ] {
        assert!(
            classify_context(code, code.len()) == TextContext::Code,
            "{code}"
        );
    }
}
