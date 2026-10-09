//! Personal, on-device autocomplete for Jot.
//!
//! # Design
//!
//! Suggestions come from an n-gram model learned online from the text the
//! user has typed, kept on their computer:
//!
//! 1. **A calibrated probability.** For the word being typed, or the next
//!    word after a space, the model estimates the probability that a word is
//!    the one coming, from the one or two words before it in the sentence and
//!    whether the sentence starts there. A suggestion shows when that
//!    probability is at least one half, so it is right more often than not.
//!    See [`model`].
//!
//! 2. **Words past the current one when they are likely too.** Words are
//!    added after the first while the chance that the whole suggestion is
//!    right stays high, and the length offered is the one that saves the
//!    most keystrokes on average.
//!
//! 3. **Words in any case.** "Thanks" and "thanks" are one word. A
//!    suggestion keeps the letters typed and finishes the word in the form
//!    the user usually writes it. See [`vocabulary`].
//!
//! 4. **Context awareness.** A language-agnostic classifier scores nearby
//!    lines as code or prose using structural features against the strongest
//!    prose signal, English function-word ratio. In prose context,
//!    code-shaped candidates are filtered out. In code context, suggestions
//!    come from the local document only and are never extended.
//!
//! The local per-document index adds the document's own words, such as
//! pasted names and code identifiers, and rebuilds on a time throttle rather
//! than on every keystroke. It counts only what the shared vocabulary
//! doesn't already hold, so no text counts twice (see
//! [`vocabulary::LocalIndex`]).
//!
//! # Learning
//!
//! The persistent vocabulary learns only from typed text. A sentence is
//! learned at the first typed key after its terminator, a line break
//! included, so a line ended by a blank line counts. It is skipped when any
//! of it was pasted, undone or redone, or inserted by the program rather than
//! typed (see [`typed`]), and when its context classifies as code. Each
//! sentence is learned once, however the document around it changes, and
//! the text a document opens with isn't learned again (see [`learned`]).
//!
//! An apostrophe between two letters stays inside a word, so contractions
//! such as "don't" are one word. One-character English words `a`, `A`, and
//! `I` are preserved. Other rejected tokens break the n-gram sequence so
//! words on either side never become falsely adjacent.
//!
//! # Spelling hygiene
//!
//! When a dictionary is available, words that are not dictionary-correct are
//! only suggested after repeated committed use, so one-off typos do not
//! surface while genuine jargon can earn trust.
//!
//! # Pacing
//!
//! When a suggestion shows, as opposed to which one, follows the user's
//! [`AutocompleteMode`]: in Quiet, the default, after a pause in typing, and
//! less often for words the user types past. See [`pacing`].

mod learned;
mod model;
mod pacing;
mod typed;
mod vocabulary;

pub use pacing::{AutocompleteMode, SuggestionPacing};
pub use vocabulary::SharedVocabulary;

use gpui_kit::component::input::{
    Suggestion, SuggestionEvent, SuggestionProvider, SuggestionRequest, TextChange,
};
use gpui_kit::{App, Task, Window};
use learned::LearnedSentences;
use model::generate_suggestion;
use pacing::Pacer;
use std::cell::RefCell;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};
use typed::TypedText;
use vocabulary::{LocalIndex, WordTally, tally};

/// Current persisted model schema and tokenization version.
const MODEL_VERSION: u32 = 2;

/// Minimum length of an ordinary learnable word.
const MIN_WORD_LEN: usize = 2;

/// Maximum length of a learnable word.
const MAX_WORD_LEN: usize = 60;

/// Persisted vocabulary size cap.
const VOCAB_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// The most occurrences of one word the open document adds to the
/// vocabulary's count, so one long document doesn't swamp it.
const LOCAL_COUNT_CAP: u32 = 50;

/// Maximum continuation words appended after completing a typed prefix.
const PREFIX_CONTINUATION_LIMIT: usize = 2;

/// Maximum continuation words appended after an empty-prefix prediction.
const EMPTY_PREDICTION_CONTINUATION_LIMIT: usize = 1;

/// Committed occurrences required before a non-dictionary word is suggested.
const NON_DICTIONARY_MIN_SHARED_FREQ: u32 = 3;

/// In-document occurrences required before a non-dictionary word is suggested.
const NON_DICTIONARY_MIN_LOCAL_FREQ: u32 = 2;

/// Maximum length of a candidate offered in prose context.
const PROSE_MAX_CANDIDATE_LEN: usize = 24;

/// Backward scan bound when extracting a committed sentence.
const SENTENCE_SCAN_BYTES: usize = 1024;

/// Backward scan bound when locating the most recent terminator.
const TERMINATOR_SEARCH_BYTES: usize = 4096;

/// Backward scan bound when extracting previous-word context.
const CONTEXT_BYTES: usize = 512;

/// Backward scan bound for Markdown fence detection.
const FENCE_SCAN_BYTES: usize = 4096;

/// Byte budget for classifying lines above the cursor.
const CLASSIFY_SCAN_BYTES: usize = 2048;

/// Byte budget for classifying lines below the cursor.
const CLASSIFY_FORWARD_BYTES: usize = 1024;

/// Mean weighted line score above which context is classified as code.
const CODE_SCORE_THRESHOLD: f64 = 0.2;

/// Common English closed-class words, the prose signal of the code/prose
/// classifier.
const FUNCTION_WORDS: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "an", "in", "that", "have", "it", "for", "not", "on",
    "with", "he", "as", "you", "do", "at", "this", "but", "his", "by", "from", "they", "we", "her",
    "she", "or", "will", "my", "one", "all", "would", "there", "their", "what", "so", "if", "is",
    "was", "are", "were", "been", "has", "had", "can", "could", "should", "i", "another", "each",
    "every", "these", "those", "many", "several", "few", "both",
];

/// Canonicalizes safe single-character prediction tokens.
fn canonical_prediction_word(word: &str) -> &str {
    if word.eq_ignore_ascii_case("a") {
        "a"
    } else if word.eq_ignore_ascii_case("i") {
        "I"
    } else {
        word
    }
}

/// Hashes a string with the standard library's default hasher.
fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Clamps an index down to the nearest UTF-8 character boundary.
fn floor_char_boundary(text: &str, mut idx: usize) -> usize {
    if idx >= text.len() {
        return text.len();
    }

    while idx > 0 && !text.is_char_boundary(idx) {
        idx -= 1;
    }

    idx
}

/// Classification of the text surrounding the cursor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextContext {
    Prose,
    Code,
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
}

/// Whether the character `ch` at byte `index` of `text` belongs to a word: a
/// word character, or an apostrophe between two letters, so contractions and
/// possessives ("don't", "I'm", "Sam's") stay one word. The typographic
/// apostrophe counts too.
fn is_word_char_at(text: &str, index: usize, ch: char) -> bool {
    if is_word_char(ch) {
        return true;
    }
    (ch == '\'' || ch == '\u{2019}')
        && text[..index]
            .chars()
            .next_back()
            .is_some_and(char::is_alphabetic)
        && text[index + ch.len_utf8()..]
            .chars()
            .next()
            .is_some_and(char::is_alphabetic)
}

/// The end of the word that starts at byte `start` of `text`.
fn word_end(text: &str, start: usize) -> usize {
    text[start..]
        .char_indices()
        .find(|&(index, character)| !is_word_char_at(text, start + index, character))
        .map_or(text.len(), |(index, _)| start + index)
}

fn is_sentence_terminator(ch: char) -> bool {
    ch == '.' || ch == '!' || ch == '?' || ch == '\n'
}

/// Returns whether a token belongs in the learned word and n-gram stream.
fn is_learnable_word(word: &str) -> bool {
    let character_count = word.chars().count();

    if character_count == 1 {
        return matches!(word, "a" | "A" | "I");
    }

    (MIN_WORD_LEN..=MAX_WORD_LEN).contains(&character_count)
        && word.chars().any(|character| character.is_alphabetic())
}

/// Commits a token to the current n-gram sequence or breaks the sequence when
/// the token is not learnable.
fn commit_extracted_word<'a>(
    text: &'a str,
    start: usize,
    end: usize,
    sequences: &mut Vec<Vec<&'a str>>,
    current: &mut Vec<&'a str>,
) {
    let word = &text[start..end];

    if is_learnable_word(word) {
        current.push(word);
    } else if !current.is_empty() {
        sequences.push(std::mem::take(current));
    }
}

/// Extracts learnable words grouped into contiguous n-gram sequences.
fn extract_sentences_and_words(text: &str, cursor_offset: Option<usize>) -> Vec<Vec<&str>> {
    let mut sequences = Vec::new();
    let mut current = Vec::new();
    let mut word_start = None;

    for (index, character) in text.char_indices() {
        if is_word_char_at(text, index, character) {
            if word_start.is_none() {
                word_start = Some(index);
            }
            continue;
        }

        if let Some(start) = word_start.take() {
            let skip = cursor_offset.is_some_and(|cursor| cursor >= start && cursor <= index);

            if skip {
                if !current.is_empty() {
                    sequences.push(std::mem::take(&mut current));
                }
            } else {
                commit_extracted_word(text, start, index, &mut sequences, &mut current);
            }
        }

        if is_sentence_terminator(character) && !current.is_empty() {
            sequences.push(std::mem::take(&mut current));
        }
    }

    if let Some(start) = word_start {
        let end = text.len();
        let skip = cursor_offset.is_some_and(|cursor| cursor >= start && cursor <= end);

        if skip {
            if !current.is_empty() {
                sequences.push(std::mem::take(&mut current));
            }
        } else {
            commit_extracted_word(text, start, end, &mut sequences, &mut current);
        }
    }

    if !current.is_empty() {
        sequences.push(current);
    }

    sequences
}

/// The words of `text`, as byte ranges.
fn word_tokens(text: &str) -> Vec<Range<usize>> {
    let mut tokens = Vec::new();
    let mut start = None;
    for (index, character) in text.char_indices() {
        if is_word_char_at(text, index, character) {
            start.get_or_insert(index);
        } else if let Some(start) = start.take() {
            tokens.push(start..index);
        }
    }
    if let Some(start) = start {
        tokens.push(start..text.len());
    }
    tokens
}

/// The learnable words of a sentence, in runs that a token that isn't
/// learnable, such as a number, ends. Each run says whether it opens the
/// sentence.
fn sentence_runs(sentence: &str) -> Vec<(bool, Vec<&str>)> {
    let mut runs = Vec::new();
    let mut current = Vec::new();
    let mut current_opens = false;
    for (index, range) in word_tokens(sentence).into_iter().enumerate() {
        let word = &sentence[range];
        if is_learnable_word(word) {
            if current.is_empty() {
                current_opens = index == 0;
            }
            current.push(word);
        } else if !current.is_empty() {
            runs.push((current_opens, std::mem::take(&mut current)));
        }
    }
    if !current.is_empty() {
        runs.push((current_opens, current));
    }
    runs
}

fn extract_prefix(text: &str, offset: usize) -> &str {
    if offset == 0 || offset > text.len() {
        return "";
    }

    let before = &text[..offset];
    let start = before
        .char_indices()
        .rev()
        .take_while(|&(index, character)| is_word_char_at(before, index, character))
        .last()
        .map(|(index, _)| index)
        .unwrap_or(offset);

    &text[start..offset]
}

/// Returns the previous word and the word before it without crossing a
/// sentence boundary.
fn previous_two_words(text: &str, from: usize) -> (Option<&str>, Option<&str>) {
    if from == 0 || from > text.len() {
        return (None, None);
    }

    let before = &text[..from];
    let sentence_start = before
        .rfind(|character: char| is_sentence_terminator(character))
        .map(|index| index + 1)
        .unwrap_or(0);

    let mut area = &before[sentence_start..];

    if area.len() > CONTEXT_BYTES {
        let mut boundary = area.len() - CONTEXT_BYTES;
        while !area.is_char_boundary(boundary) {
            boundary += 1;
        }
        area = &area[boundary..];
    }

    let mut last = None;
    let mut previous = None;
    let mut current = None;

    for (index, character) in area.char_indices() {
        if is_word_char_at(area, index, character) {
            if current.is_none() {
                current = Some(index);
            }
        } else if let Some(start) = current.take() {
            previous = last;
            last = Some((start, index));
        }
    }

    if let Some(start) = current {
        previous = last;
        last = Some((start, area.len()));
    }

    (
        last.map(|(start, end)| &area[start..end]),
        previous.map(|(start, end)| &area[start..end]),
    )
}

/// Checks whether any non-whitespace content exists after the cursor on the
/// current line.
fn has_text_after_cursor_on_line(text: &str, offset: usize) -> bool {
    if offset >= text.len() {
        return false;
    }

    text[offset..]
        .chars()
        .take_while(|&character| character != '\n')
        .any(|character| !character.is_whitespace())
}

fn is_closing_punctuation(character: char) -> bool {
    matches!(
        character,
        ')' | ']' | '}' | '\'' | '"' | '\u{2019}' | '\u{201D}'
    )
}

/// Checks whether a closing enclosure appears before the prefix start.
fn has_closing_punctuation_before(text: &str, prefix_start: usize) -> bool {
    if prefix_start == 0 {
        return false;
    }

    for character in text[..prefix_start].chars().rev() {
        if character.is_whitespace() {
            continue;
        }

        return is_closing_punctuation(character);
    }

    false
}

fn line_start(text: &str, offset: usize) -> usize {
    text[..offset]
        .rfind('\n')
        .map(|index| index + 1)
        .unwrap_or(0)
}

/// Detects whether the cursor sits inside a Markdown code fence.
fn inside_code_fence(text: &str, offset: usize) -> bool {
    let floor = floor_char_boundary(text, offset.saturating_sub(FENCE_SCAN_BYTES));

    let region_start = if floor == 0 {
        0
    } else {
        match text[floor..offset].find('\n') {
            Some(index) => floor + index + 1,
            None => return false,
        }
    };

    let region = &text[region_start..line_start(text, offset)];
    let fences = region
        .lines()
        .filter(|line| line.trim_start().starts_with("```"))
        .count();

    fences % 2 == 1
}

/// Strips Markdown block markers from the start of a line.
fn strip_markup_prefix(text: &str) -> &str {
    let mut text = text;

    loop {
        let trimmed = text.trim_start();
        let mut characters = trimmed.chars();

        match characters.next() {
            Some('#') | Some('>') => {
                text = &trimmed[1..];
            }
            Some('-') | Some('*') | Some('+') if characters.next() == Some(' ') => {
                text = &trimmed[2..];
            }
            Some(character) if character.is_ascii_digit() => {
                let after_digits =
                    trimmed.trim_start_matches(|candidate: char| candidate.is_ascii_digit());

                match after_digits.strip_prefix(". ") {
                    Some(rest) => text = rest,
                    None => return trimmed,
                }
            }
            _ => return trimmed,
        }
    }
}

fn is_code_symbol(character: char) -> bool {
    matches!(
        character,
        '{' | '}'
            | '['
            | ']'
            | '('
            | ')'
            | '<'
            | '>'
            | '='
            | ';'
            | ':'
            | '&'
            | '|'
            | '+'
            | '*'
            | '/'
            | '\\'
            | '^'
            | '%'
            | '~'
            | '`'
            | '@'
            | '$'
            | '#'
    )
}

fn is_function_word(word: &str) -> bool {
    FUNCTION_WORDS
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

/// Tests for identifier-shaped tokens.
fn looks_like_identifier(token: &str) -> bool {
    if token.len() < 2 {
        return false;
    }

    if token.contains("::") || token.contains("->") || token.contains("=>") || token.contains("()")
    {
        return true;
    }

    let mut has_alpha = false;
    let mut has_digit = false;
    let mut has_underscore = false;
    let mut has_lower = false;
    let mut internal_upper = false;

    for (index, character) in token.chars().enumerate() {
        if character == '_' {
            has_underscore = true;
        }

        if character.is_ascii_digit() {
            has_digit = true;
        }

        if character.is_alphabetic() {
            has_alpha = true;

            if character.is_lowercase() {
                has_lower = true;
            } else if index > 0 {
                internal_upper = true;
            }
        }
    }

    (has_underscore && has_alpha) || (has_digit && has_alpha) || (internal_upper && has_lower)
}

/// Scores a line on a code-versus-prose axis in `[-1, 1]`.
pub(crate) fn line_code_score(line: &str) -> f64 {
    let indented = line.starts_with('\t') || line.starts_with("    ");
    let stripped = strip_markup_prefix(line).trim_end();

    if stripped.chars().count() < 3 {
        return 0.0;
    }

    let mut symbols = 0;
    let mut nonspace = 0;

    let mut characters = stripped.chars().peekable();
    while let Some(character) = characters.next() {
        if character.is_whitespace() {
            continue;
        }

        nonspace += 1;

        // A colon before a space is prose punctuation, as in "Agenda: budget,
        // hiring". Code puts one before a space too, in a type or a key, but
        // has other symbols to tell it by.
        let prose_colon =
            character == ':' && characters.peek().is_some_and(|next| next.is_whitespace());
        if is_code_symbol(character) && !prose_colon {
            symbols += 1;
        }
    }

    let mut tokens = 0;
    let mut identifier_tokens = 0;
    let mut function_words = 0;

    for token in stripped.split_whitespace() {
        tokens += 1;

        if looks_like_identifier(token) {
            identifier_tokens += 1;
        }

        let core = token.trim_matches(|character: char| !character.is_alphabetic());

        if !core.is_empty() && is_function_word(core) {
            function_words += 1;
        }
    }

    if tokens == 0 || nonspace == 0 {
        return 0.0;
    }

    let symbol_ratio = symbols as f64 / nonspace as f64;
    let identifier_ratio = identifier_tokens as f64 / tokens as f64;
    let function_word_ratio = function_words as f64 / tokens as f64;

    let mut score = 0.0;
    score += 2.0 * (symbol_ratio / 0.15).min(1.0);
    score += 1.5 * identifier_ratio;
    score -= 1.6 * (function_word_ratio / 0.35).min(1.0);

    if matches!(stripped.chars().last(), Some(';' | '{' | '}')) {
        score += 0.6;
    }

    if indented {
        score += 0.25;
    }

    score.clamp(-1.0, 1.0)
}

/// Classifies the cursor surroundings as code or prose.
fn classify_context(text: &str, offset: usize) -> TextContext {
    if inside_code_fence(text, offset) {
        return TextContext::Code;
    }

    let mut score = 0.0;
    let mut weight = 0.0;

    let current_start = line_start(text, offset);
    let current = &text[current_start..offset];

    if !current.trim().is_empty() {
        score += 3.0 * line_code_score(current);
        weight += 3.0;
    }

    let above_weights = [2.0, 1.5, 1.0, 0.75, 0.5, 0.5];
    let mut taken = 0;
    let mut scanned = 0;
    let mut end = current_start;

    while end > 0 && taken < above_weights.len() && scanned < CLASSIFY_SCAN_BYTES {
        let start = line_start(text, end - 1);
        let line = &text[start..end - 1];
        scanned += end - start;

        if !line.trim().is_empty() {
            score += above_weights[taken] * line_code_score(line);
            weight += above_weights[taken];
            taken += 1;
        }

        end = start;
    }

    let below_weights = [1.0, 0.75];
    let mut taken = 0;
    let mut scanned = 0;
    let mut position = text[offset..].find('\n').map(|index| offset + index + 1);

    while let Some(start) = position {
        if taken >= below_weights.len() || scanned > CLASSIFY_FORWARD_BYTES || start >= text.len() {
            break;
        }

        let end = text[start..]
            .find('\n')
            .map(|index| start + index)
            .unwrap_or(text.len());

        let line = &text[start..end];
        scanned += end - start;

        if !line.trim().is_empty() {
            score += below_weights[taken] * line_code_score(line);
            weight += below_weights[taken];
            taken += 1;
        }

        position = if end < text.len() {
            Some(end + 1)
        } else {
            None
        };
    }

    if weight == 0.0 {
        return TextContext::Prose;
    }

    if score / weight > CODE_SCORE_THRESHOLD {
        TextContext::Code
    } else {
        TextContext::Prose
    }
}

/// Rejects code-shaped candidates in prose context.
fn is_prose_shaped(word: &str) -> bool {
    if word.chars().count() > PROSE_MAX_CANDIDATE_LEN {
        return false;
    }

    let mut has_lower = false;
    let mut internal_upper = false;

    for (index, character) in word.chars().enumerate() {
        if character == '_' || character.is_ascii_digit() {
            return false;
        }

        if character.is_uppercase() {
            if index > 0 {
                internal_upper = true;
            }
        } else if character.is_lowercase() {
            has_lower = true;
        }
    }

    !(internal_upper && has_lower)
}

/// Inline completion provider backed by shared and local learned vocabulary.
pub struct JotCompletionProvider {
    shared_vocab: Rc<RefCell<SharedVocabulary>>,
    local_index: RefCell<LocalIndex>,
    learned: RefCell<LearnedSentences>,
    /// The words this document taught the shared vocabulary.
    taught: RefCell<WordTally>,
    /// The words the document opened with.
    opened: RefCell<WordTally>,
    typed: RefCell<TypedText>,
    pacer: Pacer,
}

impl JotCompletionProvider {
    pub fn new(shared_vocab: Rc<RefCell<SharedVocabulary>>, pacing: Rc<SuggestionPacing>) -> Self {
        Self {
            shared_vocab,
            local_index: RefCell::new(LocalIndex::new()),
            learned: RefCell::new(LearnedSentences::default()),
            taught: RefCell::new(WordTally::new()),
            opened: RefCell::new(WordTally::new()),
            typed: RefCell::new(TypedText::default()),
            pacer: Pacer::new(pacing),
        }
    }

    /// Notes the text a document opened with. It isn't the user's typing in
    /// this session, so none of it is learned.
    pub fn opened_with(&self, text: &str) {
        self.learned.borrow_mut().opened_with(text);
        *self.opened.borrow_mut() = tally(text);
    }

    /// Hears that the user took a suggestion with Tab or closed it with
    /// Escape. The document forwards its text editor's events here.
    pub fn suggestion_event(&self, event: &SuggestionEvent, cx: &App) {
        self.pacer.event(event, cx.background_executor().now());
    }

    /// Learns the most recently finished prose sentence before the cursor,
    /// when the user typed all of it and it isn't learned already.
    fn learn_preceding_sentence(&self, text: &str, offset: usize) {
        let Some((range, terminator_offset)) = preceding_sentence(text, offset) else {
            return;
        };

        {
            let typed = self.typed.borrow();
            if typed.overlaps_untyped(range.clone())
                || self
                    .learned
                    .borrow_mut()
                    .is_learned(text, range.clone(), &typed)
            {
                return;
            }
        }

        if classify_context(text, terminator_offset) == TextContext::Code {
            return;
        }

        let words = self
            .shared_vocab
            .borrow_mut()
            .learn_sentence(&text[range.clone()]);
        if words.is_empty() {
            return;
        }

        let mut taught = self.taught.borrow_mut();
        for word in words {
            *taught.entry(word.into()).or_default() += 1;
        }
        self.learned.borrow_mut().learned(text, range);
        // The index counted the sentence as the document's own.
        self.local_index.borrow_mut().mark_stale();
    }

    /// Brings the index of the document's words up to date, at most every
    /// couple of seconds.
    fn refresh_local_index(&self, text: &str, cursor: usize, now: Instant) {
        let shared = self.shared_vocab.borrow();
        self.local_index.borrow_mut().rebuild_if_stale(
            text,
            cursor,
            now,
            shared.counts(),
            &self.taught.borrow(),
            &self.opened.borrow(),
        );
    }
}

/// The last finished sentence before `offset`, as its trimmed byte range and
/// the offset of the terminator after it.
///
/// A sentence ends at a terminator: `.`, `!`, `?` or a line break. Empty
/// stretches between terminators are passed over, so the line before a blank
/// line counts as finished.
fn preceding_sentence(text: &str, offset: usize) -> Option<(Range<usize>, usize)> {
    let search_floor = floor_char_boundary(text, offset.saturating_sub(TERMINATOR_SEARCH_BYTES));
    let mut end = offset;
    loop {
        let terminator = search_floor + text[search_floor..end].rfind(is_sentence_terminator)?;
        let scan_floor = floor_char_boundary(text, terminator.saturating_sub(SENTENCE_SCAN_BYTES));
        let start = text[scan_floor..terminator]
            .rfind(is_sentence_terminator)
            .map_or(scan_floor, |index| scan_floor + index + 1);
        let segment = &text[start..terminator];
        let sentence = segment.trim();
        if !sentence.is_empty() {
            let sentence_start = start + segment.len() - segment.trim_start().len();
            return Some((sentence_start..sentence_start + sentence.len(), terminator));
        }
        end = terminator;
    }
}

/// Offers one word, or a short run of words, to continue the text at the
/// caret. The text editor shows it as ghost text that Tab accepts.
impl SuggestionProvider for JotCompletionProvider {
    fn suggestions(
        &self,
        request: &SuggestionRequest,
        _window: &mut Window,
        cx: &mut App,
    ) -> Task<anyhow::Result<Vec<Suggestion>>> {
        if self.pacer.mode() == AutocompleteMode::Off {
            return Task::ready(Ok(Vec::new()));
        }

        let text = request.text().to_string();
        let offset = floor_char_boundary(&text, request.offset().min(text.len()));

        let now = cx.background_executor().now();
        self.learn_preceding_sentence(&text, offset);
        self.refresh_local_index(&text, offset, now);

        let suggestion = {
            let shared = self.shared_vocab.borrow();
            let local = self.local_index.borrow();

            generate_suggestion(&shared, &local, &text, offset)
        };
        let suggestion = self.pacer.offer(&text, offset, suggestion, now);
        self.typed
            .borrow_mut()
            .offered(offset, suggestion.as_deref());

        Task::ready(Ok(suggestion.into_iter().map(Suggestion::new).collect()))
    }

    fn is_trigger(&self, offset: usize, text: &str, cx: &mut App) -> bool {
        self.typed.borrow_mut().typed(offset, text);
        let triggers = self.pacer.mode() != AutocompleteMode::Off
            && text.chars().any(|character| {
                character.is_alphanumeric()
                    || character == '_'
                    || character == '-'
                    || character == ' '
                    || is_sentence_terminator(character)
            });
        self.pacer
            .typed(offset, text, triggers, cx.background_executor().now());
        triggers
    }

    /// Quiet waits for typing to pause, longer after ignored suggestions,
    /// but not while the user types along the suggestion on screen. Eager
    /// never waits.
    fn debounce(&self) -> Duration {
        self.pacer.debounce()
    }

    /// Notes which inserted text the user didn't type, in every mode.
    ///
    /// A request learns the sentence before the caret, and Eager makes one
    /// after every keystroke that triggers. Quiet makes one only once typing
    /// pauses, so it learns after those keystrokes here instead. It learns
    /// what Eager would, and at the same moments: after a typed key, never
    /// at a paste, an undo or another edit the user didn't type.
    fn did_change(&self, change: &TextChange, cx: &mut App) {
        self.typed.borrow_mut().changed(change);
        let now = cx.background_executor().now();
        if let Some(caret) = self.pacer.changed(change, now) {
            let text = change.text().to_string();
            let caret = floor_char_boundary(&text, caret.min(text.len()));
            self.learn_preceding_sentence(&text, caret);
        }
    }
}

#[cfg(test)]
mod tests;
