//! Which text in a document the user typed.
//!
//! The persistent vocabulary learns only from typing. The text editor reports
//! what the user typed through `is_trigger`, and every change to the text
//! afterwards through `did_change`. An inserted range is *untyped* when no
//! keystroke reported it: a paste, an undo or redo, or any other edit the
//! program made. These ranges move with later edits, and a sentence that
//! overlaps one is never learned.
//!
//! Three kinds of insertion count as typing:
//!
//! - The keystroke the editor just reported, with whatever the editor added
//!   to it, such as the closing bracket of a typed opening one.
//! - A suggestion taken with Tab, or a word of it taken on its own, since it
//!   came from the user's own writing.
//! - One word that replaces another, such as a spelling correction.
//!
//! Insertions without a letter, such as Enter or an indent, can't add words
//! and are left out.

use gpui_kit::component::input::TextChange;
use std::ops::Range;

#[derive(Default)]
pub(super) struct TypedText {
    /// Byte ranges of untyped insertions in the current text, sorted and
    /// apart.
    untyped: Vec<Range<usize>>,
    /// Keystrokes reported since the last change, as (offset, text).
    keystrokes: Vec<(usize, String)>,
    /// The suggestion on screen, as (caret, ghost text).
    offered: Option<(usize, String)>,
}

impl TypedText {
    /// The user typed `text` at byte `offset`. Its edit follows in the next
    /// change.
    pub fn typed(&mut self, offset: usize, text: &str) {
        self.keystrokes.push((offset, text.to_string()));
    }

    /// The suggestion offered at `caret`, or none.
    pub fn offered(&mut self, caret: usize, ghost: Option<&str>) {
        self.offered = ghost
            .filter(|ghost| !ghost.is_empty())
            .map(|ghost| (caret, ghost.to_string()));
    }

    /// Follows `change`, edit by edit.
    pub fn changed(&mut self, change: &TextChange) {
        for edit in change.edits() {
            self.edit(edit.range(), edit.text());
        }
        self.keystrokes.clear();
    }

    fn edit(&mut self, range: Range<usize>, inserted: &str) {
        let typed = self
            .keystrokes
            .iter()
            .position(|(offset, text)| {
                *offset == range.start && inserted.starts_with(text.as_str())
            })
            .map(|index| self.keystrokes.remove(index))
            .is_some();
        // Taking the suggestion inserts it at the caret, or some of it.
        // After a word of it was taken, taking the rest rewrites that word
        // with the whole of it.
        let accepted = !typed
            && matches!(&self.offered, Some((caret, ghost))
                if range.end == *caret
                    && !inserted.is_empty()
                    && ((range.is_empty() && ghost.starts_with(inserted))
                        || inserted.ends_with(ghost.as_str())));
        // Taking one word of a suggestion leaves the rest of it offered.
        self.offered = match self.offered.take() {
            Some((caret, ghost))
                if accepted && range.is_empty() && ghost.len() > inserted.len() =>
            {
                Some((caret + inserted.len(), ghost[inserted.len()..].to_string()))
            }
            _ => None,
        };
        let correction = !range.is_empty() && is_one_word(inserted);

        self.replace(range.clone(), inserted.len());
        if !(typed || accepted || correction) && inserted.chars().any(char::is_alphabetic) {
            self.insert(range.start..range.start + inserted.len());
        }
    }

    /// Moves the untyped ranges for `range` replaced with `new_len` bytes,
    /// dropping what it removed.
    fn replace(&mut self, range: Range<usize>, new_len: usize) {
        let shift = |offset: usize| offset - range.len() + new_len;
        let mut moved: Vec<Range<usize>> = Vec::with_capacity(self.untyped.len() + 1);
        let mut push = |piece: Range<usize>| match moved.last_mut() {
            // A deletion inside a range leaves its two ends touching.
            Some(last) if last.end >= piece.start => last.end = last.end.max(piece.end),
            _ => moved.push(piece),
        };
        for untyped in std::mem::take(&mut self.untyped) {
            if untyped.end <= range.start {
                push(untyped);
            } else if untyped.start >= range.end {
                push(shift(untyped.start)..shift(untyped.end));
            } else {
                if untyped.start < range.start {
                    push(untyped.start..range.start);
                }
                if untyped.end > range.end {
                    push(shift(range.end)..shift(untyped.end));
                }
            }
        }
        self.untyped = moved;
    }

    fn insert(&mut self, range: Range<usize>) {
        let index = self
            .untyped
            .partition_point(|untyped| untyped.end < range.start);
        let mut merged = range;
        while index < self.untyped.len() && self.untyped[index].start <= merged.end {
            let next = self.untyped.remove(index);
            merged = merged.start.min(next.start)..merged.end.max(next.end);
        }
        self.untyped.insert(index, merged);
    }

    /// Whether any of `range` was inserted without typing.
    pub fn overlaps_untyped(&self, range: Range<usize>) -> bool {
        let index = self
            .untyped
            .partition_point(|untyped| untyped.end <= range.start);
        self.untyped
            .get(index)
            .is_some_and(|untyped| untyped.start < range.end)
    }
}

/// Whether `text` is a single word, with no spaces or punctuation around it.
fn is_one_word(text: &str) -> bool {
    !text.is_empty()
        && text.chars().count() <= super::MAX_WORD_LEN
        && text.chars().all(|character| {
            character.is_alphanumeric() || matches!(character, '\'' | '\u{2019}' | '-')
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn untyped(typed: &TypedText) -> Vec<(usize, usize)> {
        typed
            .untyped
            .iter()
            .map(|range| (range.start, range.end))
            .collect()
    }

    #[test]
    fn pasted_ranges_move_with_later_edits() {
        let mut typed = TypedText::default();
        typed.edit(0..0, "Hello world");
        assert_eq!(untyped(&typed), [(0, 11)]);

        // Typed text before it moves it, typed text inside splits it.
        typed.typed(0, "A");
        typed.edit(0..0, "A");
        assert_eq!(untyped(&typed), [(1, 12)]);
        typed.typed(6, " ");
        typed.edit(6..6, " ");
        assert_eq!(untyped(&typed), [(1, 6), (7, 13)]);

        // Deleting part of it shortens it, deleting all of it drops it.
        typed.edit(8..10, "");
        assert_eq!(untyped(&typed), [(1, 6), (7, 11)]);
        typed.edit(0..20, "");
        assert_eq!(untyped(&typed), Vec::<(usize, usize)>::new());
    }

    #[test]
    fn neighbouring_pastes_merge() {
        let mut typed = TypedText::default();
        typed.edit(0..0, "one two");
        typed.edit(7..7, " three");
        typed.edit(20..20, "four");
        assert_eq!(untyped(&typed), [(0, 13), (20, 24)]);
        assert!(typed.overlaps_untyped(12..15));
        assert!(!typed.overlaps_untyped(13..20));
        assert!(typed.overlaps_untyped(15..21));
        assert!(!typed.overlaps_untyped(24..30));
    }

    #[test]
    fn typing_tab_and_corrections_count_as_typed() {
        let none = Vec::<(usize, usize)>::new();
        let mut typed = TypedText::default();
        typed.typed(0, "(");
        typed.edit(0..0, "()");
        typed.offered(1, Some("llo world"));
        typed.edit(1..1, "llo");
        assert_eq!(untyped(&typed), none, "a word of the suggestion");
        typed.edit(1..4, "llo world");
        assert_eq!(untyped(&typed), none, "the rest of it, over that word");
        typed.edit(10..10, " again");
        assert_eq!(untyped(&typed), [(10, 16)], "nothing is offered any more");
        typed.edit(0..3, "Hullo");
        assert_eq!(untyped(&typed), [(12, 18)], "a one-word correction");
        typed.edit(18..18, "\n    ");
        assert_eq!(untyped(&typed), [(12, 18)], "no letters");

        typed.offered(18, Some(" more words"));
        typed.edit(18..18, " more words");
        assert_eq!(untyped(&typed), [(12, 18)], "a whole suggestion");
    }
}
