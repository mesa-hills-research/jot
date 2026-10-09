//! Which sentences of a document the vocabulary has learned.
//!
//! A sentence is known by its content: a hash of its trimmed text, counted
//! once for each time it was learned. Content survives edits elsewhere in the
//! document, where a position would move, so an edit above a learned
//! sentence doesn't make it new again. A sentence typed twice is learned
//! twice: at a learning moment, a sentence learned *n* times is learned again
//! only when the document holds more than *n* typed copies of it.
//!
//! The text a document opens with is counted as learned, whether or not it
//! ever was, so reopening a document teaches nothing. Changing a sentence
//! makes it a different one, which is learned when the user next finishes it.

use super::typed::TypedText;
use super::{hash_str, is_sentence_terminator};
use std::collections::HashMap;
use std::ops::Range;

#[derive(Default)]
pub(super) struct LearnedSentences {
    /// How many copies of each sentence, by hash, count as learned.
    counts: HashMap<u64, u32>,
    /// The end and hash of the sentence the last check found learned. The
    /// next keystrokes check the same one, and skip counting its copies.
    last_known: Option<(usize, u64)>,
}

impl LearnedSentences {
    /// Counts every sentence of `text`, which a document opened with, as
    /// learned. The last one counts even without a terminator after it.
    pub fn opened_with(&mut self, text: &str) {
        for sentence in text.split(is_sentence_terminator) {
            let sentence = sentence.trim();
            if !sentence.is_empty() {
                *self.counts.entry(hash_str(sentence)).or_default() += 1;
            }
        }
    }

    /// Whether the sentence at `range` of `text` counts as learned already.
    pub fn is_learned(&mut self, text: &str, range: Range<usize>, typed: &TypedText) -> bool {
        let sentence = &text[range.clone()];
        let hash = hash_str(sentence);
        let learned = self.counts.get(&hash).copied().unwrap_or(0);
        if learned == 0 {
            return false;
        }
        if self.last_known == Some((range.end, hash)) {
            return true;
        }
        let copies = typed_copies(text, sentence, typed, learned as usize + 1);
        if copies > learned as usize {
            return false;
        }
        self.last_known = Some((range.end, hash));
        true
    }

    /// Records that the sentence at `range` of `text` was learned.
    pub fn learned(&mut self, text: &str, range: Range<usize>) {
        let hash = hash_str(&text[range.clone()]);
        *self.counts.entry(hash).or_default() += 1;
        self.last_known = Some((range.end, hash));
    }
}

/// How many finished, typed copies of `sentence` `text` holds, counting up to
/// `limit`.
fn typed_copies(text: &str, sentence: &str, typed: &TypedText, limit: usize) -> usize {
    text.match_indices(sentence)
        .filter(|&(start, _)| {
            let end = start + sentence.len();
            starts_sentence(&text[..start])
                && ends_sentence(&text[end..])
                && !typed.overlaps_untyped(start..end)
        })
        .take(limit)
        .count()
}

/// Whether text ending with `before` is at the start of a sentence.
fn starts_sentence(before: &str) -> bool {
    for character in before.chars().rev() {
        if is_sentence_terminator(character) {
            return true;
        }
        if !character.is_whitespace() {
            return false;
        }
    }
    true
}

/// Whether `after` begins with the end of a sentence: spaces, then a
/// terminator.
fn ends_sentence(after: &str) -> bool {
    after
        .chars()
        .find(|character| is_sentence_terminator(*character) || !character.is_whitespace())
        .is_some_and(is_sentence_terminator)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learned_at(learned: &mut LearnedSentences, text: &str, sentence: &str) -> bool {
        let start = text.rfind(sentence).unwrap();
        learned.is_learned(text, start..start + sentence.len(), &TypedText::default())
    }

    #[test]
    fn a_sentence_is_known_by_its_content_and_copies() {
        let mut learned = LearnedSentences::default();
        let text = "Ferns grow slowly. Mosses grow.";
        assert!(!learned_at(&mut learned, text, "Ferns grow slowly"));
        learned.learned(text, 0..17);
        assert!(learned_at(&mut learned, text, "Ferns grow slowly"));

        // Moved by an edit above, it is the same sentence.
        let edited = "A\nFerns grow slowly. Mosses grow.";
        assert!(learned_at(&mut learned, edited, "Ferns grow slowly"));

        // A second typed copy is new, and a copy inside a longer sentence
        // isn't one.
        let twice = "Ferns grow slowly. Ferns grow slowly! Mosses";
        assert!(!learned_at(&mut learned, twice, "Ferns grow slowly"));
        let inside = "Ferns grow slowly. Old Ferns grow slowly too.";
        assert!(learned_at(&mut learned, inside, "Ferns grow slowly"));
    }

    #[test]
    fn the_text_a_document_opens_with_counts_as_learned() {
        let mut learned = LearnedSentences::default();
        learned.opened_with("Ferns grow slowly.\n\nMosses grow faster. Lichens wait");
        let text = "Ferns grow slowly.\n\nMosses grow faster. Lichens wait.";
        assert!(learned_at(&mut learned, text, "Mosses grow faster"));
        assert!(learned_at(&mut learned, text, "Lichens wait"));
        assert!(!learned_at(
            &mut learned,
            "Lichens wait patiently.",
            "Lichens wait patiently"
        ));
    }
}
