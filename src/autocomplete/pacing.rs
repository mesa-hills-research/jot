//! When jot offers a word suggestion, as opposed to which one.
//!
//! The model in the parent module decides what would complete the text at the
//! caret. This module decides whether to show it now, in the mode the user
//! chose:
//!
//! - **Off** offers nothing.
//! - **Eager** offers the model's suggestion after every keystroke.
//! - **Quiet**, the default, waits for typing to pause and backs off from
//!   suggestions the user types past. Someone typing a word they know never
//!   sees a suggestion. Someone who hesitates finds one there.
//!
//! # Ignored suggestions
//!
//! A suggestion is *offered* when it shows as ghost text after the caret.
//! From then on it is *pending* until one of these settles it:
//!
//! - **Accepted.** Tab takes it. Ignored suggestions stop counting, and the
//!   next one comes after the plain pause again.
//! - **Typed along.** The user types the next letters of its first word. It
//!   stays pending, and on screen without waiting for a pause, so it doesn't
//!   blink away under keys that match it. It stays pending even when the
//!   model has nothing to show after the new letters.
//! - **Ignored.** The user types anything else at the caret: a letter the
//!   word doesn't continue with, a space or punctuation that ends the word
//!   short of it, Enter, or the word's last letter, which finishes it without
//!   Tab.
//! - **Refused.** Escape closes it. This counts as [`ESCAPE_WEIGHT`] ignored
//!   suggestions.
//! - **Left.** The user deletes, undoes, or moves the caret and types
//!   somewhere else. That says nothing about the suggestion, so it counts for
//!   nothing. jot notices a caret move when the next typing lands away from
//!   the suggestion, so moving away and back before typing counts as staying.
//!
//! An ignored or refused word is not offered again for [`SUPPRESSION`] while
//! the user types the same word from the same start: the prefix it was
//! ignored after or a longer one, or, for a word predicted after a space, the
//! same previous word. When it is the model's best answer, nothing shows.
//! Each ignored suggestion also lengthens the pause before the next one by
//! [`IGNORED_EXTRA_PAUSE`], up to [`MAX_QUIET_PAUSE`].
//!
//! This state lives in memory for the session.

use super::{extract_prefix, is_word_char, previous_two_words};
use gpui_kit::component::input::{SuggestionEvent, TextChange};
use serde::{Deserialize, Deserializer, Serialize};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// How long typing must pause before Quiet offers a suggestion. Typing a
/// familiar word leaves well under this between keys, and a hesitation lasts
/// longer.
pub const QUIET_PAUSE: Duration = Duration::from_millis(300);

/// How much longer Quiet waits for each suggestion ignored in a row, so that
/// someone who keeps typing past them sees fewer.
pub const IGNORED_EXTRA_PAUSE: Duration = Duration::from_millis(150);

/// The longest Quiet waits, however many suggestions were ignored. A real
/// hesitation still brings one up.
pub const MAX_QUIET_PAUSE: Duration = Duration::from_millis(1000);

/// How long an ignored word stays unoffered after the prefix it was ignored
/// at: the rest of a stretch of writing, after which it may be wanted again.
pub const SUPPRESSION: Duration = Duration::from_secs(10 * 60);

/// How many ignored suggestions Escape counts as. Escape is a clear no.
pub const ESCAPE_WEIGHT: u32 = 2;

/// How eagerly jot offers word suggestions.
///
/// Saved as `"off"`, `"quiet"` or `"eager"`. Settings saved when this was a
/// switch read `true` as Quiet and `false` as Off.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AutocompleteMode {
    /// No suggestions.
    Off,
    /// Suggestions after a pause in typing, fewer for words typed past.
    #[default]
    Quiet,
    /// A suggestion after every keystroke.
    Eager,
}

impl<'de> Deserialize<'de> for AutocompleteMode {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Saved {
            Switch(bool),
            Name(String),
        }
        Ok(match Saved::deserialize(deserializer)? {
            Saved::Switch(true) => Self::Quiet,
            Saved::Switch(false) => Self::Off,
            Saved::Name(name) => match name.as_str() {
                "off" => Self::Off,
                "eager" => Self::Eager,
                // A mode this version doesn't know, from a later one.
                _ => Self::default(),
            },
        })
    }
}

/// What every document's suggestions share for the session: the mode, and
/// how the user has been answering suggestions.
pub struct SuggestionPacing {
    mode: Cell<AutocompleteMode>,
    /// Ignored suggestions since the last accepted one, Escape counting
    /// [`ESCAPE_WEIGHT`].
    ignored_in_a_row: Cell<u32>,
    /// Lowercased ignored words, each with the lowercased leads it was
    /// ignored after and when that ends.
    suppressed: RefCell<HashMap<String, Vec<(String, Instant)>>>,
}

impl SuggestionPacing {
    pub fn new(mode: AutocompleteMode) -> Self {
        Self {
            mode: Cell::new(mode),
            ignored_in_a_row: Cell::new(0),
            suppressed: RefCell::new(HashMap::new()),
        }
    }

    pub fn mode(&self) -> AutocompleteMode {
        self.mode.get()
    }

    /// Switches every document to `mode` from its next keystroke.
    pub fn set_mode(&self, mode: AutocompleteMode) {
        self.mode.set(mode);
    }

    /// How long typing must pause before Quiet offers a suggestion now.
    pub fn pause(&self) -> Duration {
        let extra = IGNORED_EXTRA_PAUSE.saturating_mul(self.ignored_in_a_row.get());
        (QUIET_PAUSE + extra).min(MAX_QUIET_PAUSE)
    }

    fn accepted(&self) {
        self.ignored_in_a_row.set(0);
    }

    fn ignored(&self, offer: &Offer, weight: u32, now: Instant) {
        self.ignored_in_a_row
            .set(self.ignored_in_a_row.get().saturating_add(weight));
        let mut suppressed = self.suppressed.borrow_mut();
        suppressed.retain(|_, leads| {
            leads.retain(|(_, until)| *until > now);
            !leads.is_empty()
        });
        suppressed
            .entry(offer.word.to_lowercase())
            .or_default()
            .push((offer.lead.to_lowercase(), now + SUPPRESSION));
    }

    /// Whether `word` was ignored after `lead`, or after the start of it,
    /// recently enough that it isn't offered.
    fn is_suppressed(&self, lead: &str, word: &str, now: Instant) -> bool {
        let lead = lead.to_lowercase();
        self.suppressed
            .borrow()
            .get(&word.to_lowercase())
            .is_some_and(|leads| {
                leads
                    .iter()
                    .any(|(ignored_after, until)| *until > now && lead.starts_with(ignored_after))
            })
    }
}

/// A suggestion on screen, or being typed along.
#[derive(Clone, Debug, PartialEq)]
struct Offer {
    /// What it continued when it first showed: the typed start of the word,
    /// or for a next-word prediction, the previous word and a space.
    lead: String,
    /// Its first word, whole.
    word: String,
    /// The caret: where typing continues it.
    caret: usize,
    /// What is left to type of `word`.
    rest: String,
    /// What is left of the whole suggestion, which Tab would insert.
    ghost: String,
}

impl Offer {
    /// The offer that `suggestion`, shown at `offset` in `text`, makes.
    fn new(text: &str, offset: usize, suggestion: &str) -> Option<Self> {
        let rest: String = suggestion
            .chars()
            .take_while(|&character| is_word_char(character))
            .collect();
        if rest.is_empty() {
            return None;
        }
        let prefix = extract_prefix(text, offset);
        let lead = if prefix.is_empty() {
            format!("{} ", previous_two_words(text, offset).0?)
        } else {
            prefix.to_string()
        };
        Some(Self {
            lead,
            word: format!("{prefix}{rest}"),
            caret: offset,
            rest,
            ghost: suggestion.to_string(),
        })
    }

    /// Whether typing `text` at the caret continues the word without
    /// finishing it.
    fn is_continued_by(&self, text: &str) -> bool {
        !text.is_empty() && text.len() < self.rest.len() && self.rest.starts_with(text)
    }

    fn advance(&mut self, text: &str) {
        self.caret += text.len();
        self.rest.drain(..text.len());
        if self.ghost.starts_with(text) {
            self.ghost.drain(..text.len());
        } else {
            self.ghost.clear();
        }
    }
}

/// One document's view of its suggestions: what it offered, and what the
/// user did next.
///
/// The text editor reports typing through [`Self::typed`], then asks for
/// [`Self::debounce`] for the same keystroke. Every edit arrives later
/// through [`Self::changed`], Tab and Escape through [`Self::event`].
pub(super) struct Pacer {
    shared: Rc<SuggestionPacing>,
    offer: RefCell<Option<Offer>>,
    /// The keystroke just typed, until the change report for it arrives.
    typed: RefCell<Option<Keystroke>>,
    /// Whether the last keystroke typed along the pending suggestion.
    typing_along: Cell<bool>,
}

/// Text the user typed, as the text editor reported it.
struct Keystroke {
    offset: usize,
    text: String,
    /// Whether Eager would ask for a suggestion after it.
    triggers: bool,
}

impl Pacer {
    pub fn new(shared: Rc<SuggestionPacing>) -> Self {
        Self {
            shared,
            offer: RefCell::new(None),
            typed: RefCell::new(None),
            typing_along: Cell::new(false),
        }
    }

    pub fn mode(&self) -> AutocompleteMode {
        self.shared.mode()
    }

    /// Whether the mode is Quiet. In another mode it forgets what it was
    /// following, so that Quiet starts afresh when it comes back.
    fn is_quiet(&self) -> bool {
        if self.mode() == AutocompleteMode::Quiet {
            return true;
        }
        self.offer.take();
        self.typed.take();
        self.typing_along.set(false);
        false
    }

    /// The user typed `text` at byte `offset`. `triggers` when Eager would
    /// ask for a suggestion after it.
    pub fn typed(&self, offset: usize, text: &str, triggers: bool, now: Instant) {
        self.typing_along.set(false);
        if !self.is_quiet() {
            return;
        }
        *self.typed.borrow_mut() = Some(Keystroke {
            offset,
            text: text.to_string(),
            triggers,
        });
        let Some(mut offer) = self.offer.take() else {
            return;
        };
        if offset != offer.caret {
            // Left: the caret moved before this.
            return;
        }
        if offer.is_continued_by(text) {
            offer.advance(text);
            *self.offer.borrow_mut() = Some(offer);
            self.typing_along.set(true);
        } else {
            self.shared.ignored(&offer, 1, now);
        }
    }

    /// How long typing must pause before suggestions are requested, for the
    /// keystroke [`Self::typed`] just reported.
    pub fn debounce(&self) -> Duration {
        if self.is_quiet() && !self.typing_along.get() {
            self.shared.pause()
        } else {
            Duration::ZERO
        }
    }

    /// Settles the pending suggestion by an edit that wasn't typing: Enter,
    /// Tab, deleting or undoing.
    ///
    /// In Quiet, returns where the caret went when `change` holds a
    /// keystroke after which Eager would have asked for a suggestion, and
    /// so learned the sentence before the caret.
    pub fn changed(&self, change: &TextChange, now: Instant) -> Option<usize> {
        let mut typed = self.typed.take();
        if !self.is_quiet() {
            return None;
        }
        let mut learn_at = None;
        for edit in change.edits() {
            let range = edit.range();
            // Edits after the keystroke move the caret it left.
            if let Some(caret) = learn_at.as_mut()
                && range.end <= *caret
            {
                *caret = *caret - range.len() + edit.text().len();
            }
            if let Some(keystroke) = typed.take_if(|keystroke| {
                range.start == keystroke.offset && edit.text().starts_with(&keystroke.text)
            }) {
                if keystroke.triggers {
                    learn_at = Some(keystroke.offset + keystroke.text.len());
                }
                continue;
            }
            let Some(offer) = self.offer.take() else {
                continue;
            };
            let inserted_at_caret = range == (offer.caret..offer.caret) && !edit.text().is_empty();
            // Tab taking the suggestion inserts it, and its event follows.
            if inserted_at_caret && !offer.ghost.starts_with(edit.text()) {
                self.shared.ignored(&offer, 1, now);
            }
        }
        learn_at
    }

    /// Settles the pending suggestion by Tab or Escape.
    pub fn event(&self, event: &SuggestionEvent, now: Instant) {
        if !self.is_quiet() {
            return;
        }
        match event {
            SuggestionEvent::Accepted { .. } => {
                self.offer.take();
                self.shared.accepted();
            }
            SuggestionEvent::Dismissed { .. } => {
                if let Some(offer) = self.offer.take() {
                    self.shared.ignored(&offer, ESCAPE_WEIGHT, now);
                }
            }
            _ => {}
        }
    }

    /// What to offer of the model's `suggestion` for the caret at `offset`
    /// in `text`.
    pub fn offer(
        &self,
        text: &str,
        offset: usize,
        suggestion: Option<String>,
        now: Instant,
    ) -> Option<String> {
        if !self.is_quiet() {
            return suggestion;
        }
        let typing_along = self.typing_along.replace(false);
        let mut pending = self.offer.borrow_mut();
        let Some(suggestion) = suggestion else {
            // While the user types along a suggestion it stays pending, even
            // when the model has nothing to add at this letter.
            if !typing_along {
                *pending = None;
            }
            return None;
        };
        let Some(offer) = Offer::new(text, offset, &suggestion) else {
            *pending = None;
            return Some(suggestion);
        };
        if typing_along {
            // Keep showing the suggestion being typed along. Should the model
            // change its mind mid-word, the new word waits for a pause.
            return match pending.as_mut() {
                Some(typed_along)
                    if typed_along.caret == offset && typed_along.word == offer.word =>
                {
                    typed_along.ghost = offer.ghost;
                    Some(suggestion)
                }
                _ => None,
            };
        }
        if self.shared.is_suppressed(&offer.lead, &offer.word, now) {
            *pending = None;
            return None;
        }
        *pending = Some(offer);
        Some(suggestion)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_ignored_suggestion_lengthens_the_pause_up_to_the_cap() {
        let pacing = SuggestionPacing::new(AutocompleteMode::Quiet);
        let offer = Offer::new("he", 2, "llo").unwrap();
        let now = Instant::now();
        let mut pauses = vec![pacing.pause()];
        for _ in 0..7 {
            pacing.ignored(&offer, 1, now);
            pauses.push(pacing.pause());
        }
        let ms = |ms| Duration::from_millis(ms);
        assert_eq!(
            pauses,
            [
                ms(300),
                ms(450),
                ms(600),
                ms(750),
                ms(900),
                ms(1000),
                ms(1000),
                ms(1000)
            ]
        );
        pacing.accepted();
        assert_eq!(pacing.pause(), QUIET_PAUSE);
        pacing.ignored(&offer, ESCAPE_WEIGHT, now);
        assert_eq!(pacing.pause(), ms(600));
    }

    #[test]
    fn a_word_is_suppressed_after_its_prefix_and_longer_ones_until_it_expires() {
        let pacing = SuggestionPacing::new(AutocompleteMode::Quiet);
        let now = Instant::now();
        let offer = Offer::new("say hel", 7, "lo world").unwrap();
        assert_eq!(offer.lead, "hel");
        assert_eq!(offer.word, "hello");
        pacing.ignored(&offer, 1, now);

        assert!(pacing.is_suppressed("hel", "hello", now));
        assert!(pacing.is_suppressed("hell", "hello", now));
        assert!(pacing.is_suppressed("Hel", "Hello", now), "in any case");
        assert!(
            !pacing.is_suppressed("he", "hello", now),
            "a shorter prefix"
        );
        assert!(!pacing.is_suppressed("hel", "help", now), "another word");
        let later = now + SUPPRESSION;
        assert!(pacing.is_suppressed("hel", "hello", later - Duration::from_secs(1)));
        assert!(!pacing.is_suppressed("hel", "hello", later));
    }

    #[test]
    fn a_prediction_is_suppressed_after_the_same_previous_word() {
        let pacing = SuggestionPacing::new(AutocompleteMode::Quiet);
        let now = Instant::now();
        let offer = Offer::new("Say hello ", 10, "world again").unwrap();
        assert_eq!(offer.lead, "hello ");
        assert_eq!(offer.word, "world");
        assert_eq!(offer.rest, "world");
        assert_eq!(offer.ghost, "world again");
        pacing.ignored(&offer, 1, now);

        assert!(pacing.is_suppressed("hello ", "world", now));
        assert!(!pacing.is_suppressed("goodbye ", "world", now));
        assert!(!pacing.is_suppressed("w", "world", now), "completing it");
    }

    #[test]
    fn typing_continues_an_offer_until_the_word_is_finished() {
        let mut offer = Offer::new("hel", 3, "lo world").unwrap();
        assert!(offer.is_continued_by("l"));
        assert!(!offer.is_continued_by("lo"), "that finishes the word");
        assert!(!offer.is_continued_by("x"));
        assert!(!offer.is_continued_by(" "));
        assert!(!offer.is_continued_by(""));
        offer.advance("l");
        assert_eq!((offer.caret, offer.rest.as_str()), (4, "o"));
        assert_eq!(offer.ghost, "o world");
        assert!(!offer.is_continued_by("o"));
    }

    #[test]
    fn modes_load_from_names_and_from_the_old_switch() {
        fn load(json: &str) -> AutocompleteMode {
            serde_json::from_str(json).unwrap()
        }
        assert_eq!(load("true"), AutocompleteMode::Quiet);
        assert_eq!(load("false"), AutocompleteMode::Off);
        assert_eq!(load(r#""off""#), AutocompleteMode::Off);
        assert_eq!(load(r#""quiet""#), AutocompleteMode::Quiet);
        assert_eq!(load(r#""eager""#), AutocompleteMode::Eager);
        assert_eq!(load(r#""someday""#), AutocompleteMode::Quiet);
        for mode in [
            AutocompleteMode::Off,
            AutocompleteMode::Quiet,
            AutocompleteMode::Eager,
        ] {
            let json = serde_json::to_string(&mode).unwrap();
            assert_eq!(load(&json), mode, "{json}");
        }
        assert_eq!(
            serde_json::to_string(&AutocompleteMode::Quiet).unwrap(),
            r#""quiet""#
        );
    }
}
