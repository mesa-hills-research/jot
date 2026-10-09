//! Personal, on-device autocomplete for Jot.
//!
//! # Design
//!
//! Suggestions are produced by an online-learned n-gram model over the text
//! the user has actually typed, governed by four principles:
//!
//! 1. **Word completion first.** Candidates for the typed prefix are ranked
//!    by a blend of unigram frequency and confidence-weighted bigram/trigram
//!    evidence. Strong, concentrated contextual distributions can penalize
//!    unseen alternatives, while sparse context still backs off safely to
//!    unigram frequency.
//!
//! 2. **Confidence-gated continuation.** A suggestion is extended beyond the
//!    current word only when the continuation is near-certain in the user's
//!    own history. The winning candidate must dominate the runner-up, and
//!    each appended word must pass conditional-probability and support
//!    thresholds, with a cumulative probability floor.
//!
//! 3. **Conservative grammatical preference.** When competing candidates
//!    form an obvious singular/plural pair, nearby closed-class determiners
//!    can adjust their ranking. This is a soft preference rather than a hard
//!    grammar rule because spelling alone does not reliably identify part of
//!    speech or noun-phrase structure.
//!
//! 4. **Context awareness.** A language-agnostic classifier scores nearby
//!    lines as code or prose using structural features against the strongest
//!    prose signal, English function-word ratio. In prose context,
//!    code-shaped candidates are filtered out. In code context, suggestions
//!    come from the local document only and are never extended.
//!
//! # Learning
//!
//! The persistent vocabulary learns only from typed text. A sentence is
//! learned when a terminator is committed after it, deduplicated by sentence
//! occurrence rather than content alone, and skipped when its context
//! classifies as code.
//!
//! One-character English words `a`, `A`, and `I` are preserved. Other
//! rejected tokens break the n-gram sequence so words on either side never
//! become falsely adjacent.
//!
//! The local per-document index, which powers in-document completion
//! including pasted content and code identifiers, rebuilds on a time
//! throttle rather than on every keystroke.
//!
//! # Spelling hygiene
//!
//! When a dictionary is available, words that are not dictionary-correct are
//! only suggested after repeated committed use, so one-off typos do not
//! surface while genuine jargon can earn trust.

use crate::spell::Dictionary;
use gpui_kit::component::input::{Suggestion, SuggestionProvider, SuggestionRequest};
use gpui_kit::{App, Task, Window};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Current persisted model schema and tokenization version.
const MODEL_VERSION: u32 = 2;

/// Minimum typed-prefix length for frequency-only completion.
const MIN_PREFIX_LEN: usize = 2;

/// Minimum number of characters gathered for a completion candidate.
const MIN_CANDIDATE_SUFFIX_LEN: usize = 1;

/// Minimum combined frequency required to display a one-character suffix
/// without contextual support.
const ONE_CHAR_SUFFIX_MIN_FREQUENCY: u32 = 3;

/// Minimum length of an ordinary learnable word.
const MIN_WORD_LEN: usize = 2;

/// Maximum length of a learnable word.
const MAX_WORD_LEN: usize = 60;

/// Persisted vocabulary size cap; learning freezes beyond this.
const VOCAB_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Upper bound on candidates gathered per trie prefix lookup.
const CANDIDATE_CAP: usize = 512;

/// Weight multiplier for occurrences in the current document.
const LOCAL_FREQ_WEIGHT: f64 = 2.0;

/// Score weight for bigram context evidence.
const BIGRAM_WEIGHT: f64 = 1.5;

/// Score weight for trigram context evidence.
const TRIGRAM_WEIGHT: f64 = 2.5;

/// Combined context count required to complete a single-character prefix.
const SHORT_PREFIX_MIN_CONTEXT: u32 = 2;

/// Support at which contextual evidence begins to receive substantial weight.
const CONTEXT_RELIABILITY_SUPPORT: f64 = 3.0;

/// Minimum successor-distribution support before an unseen candidate can be
/// treated as weak contradictory evidence.
const CONTEXT_MISS_MIN_TOTAL: u32 = 3;

/// Strength of a confidence-weighted penalty for an unseen candidate.
const CONTEXT_MISS_PENALTY: f64 = 0.75;

/// Score adjustment for a candidate participating in a determiner-sensitive
/// singular/plural pair.
const NUMBER_AGREEMENT_WEIGHT: f64 = 1.25;

/// Minimum successor support for extending a suggestion.
const EXTEND_MIN_COUNT: u32 = 3;

/// Minimum conditional probability for the first appended word.
const EXTEND_FIRST_MIN_PROB: f64 = 0.6;

/// Minimum conditional probability for the second appended word.
const EXTEND_SECOND_MIN_PROB: f64 = 0.75;

/// Cumulative probability floor across all appended words.
const EXTEND_MIN_CUMULATIVE: f64 = 0.5;

/// Maximum continuation words appended after completing a typed prefix.
const PREFIX_CONTINUATION_LIMIT: usize = 2;

/// Maximum continuation words appended after an empty-prefix prediction.
const EMPTY_PREDICTION_CONTINUATION_LIMIT: usize = 1;

/// Trigram support required for an empty-prefix prediction.
const PREDICT_TRIGRAM_MIN_COUNT: u32 = 2;

/// Trigram probability required for an empty-prefix prediction.
const PREDICT_TRIGRAM_MIN_PROB: f64 = 0.5;

/// Bigram support required for an empty-prefix prediction.
const PREDICT_BIGRAM_MIN_COUNT: u32 = 4;

/// Bigram probability required for an empty-prefix prediction.
const PREDICT_BIGRAM_MIN_PROB: f64 = 0.6;

/// Committed occurrences required before a non-dictionary word is suggested.
const NON_DICTIONARY_MIN_SHARED_FREQ: u32 = 3;

/// In-document occurrences required before a non-dictionary word is suggested.
const NON_DICTIONARY_MIN_LOCAL_FREQ: u32 = 2;

/// Maximum length of a candidate offered in prose context.
const PROSE_MAX_CANDIDATE_LEN: usize = 24;

/// Minimum interval between full rebuilds of the local document index.
const LOCAL_REBUILD_INTERVAL: Duration = Duration::from_secs(2);

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

/// Common English closed-class words used both for prose classification and
/// normalization of n-gram context keys.
const FUNCTION_WORDS: &[&str] = &[
    "the", "be", "to", "of", "and", "a", "an", "in", "that", "have", "it", "for", "not", "on",
    "with", "he", "as", "you", "do", "at", "this", "but", "his", "by", "from", "they", "we", "her",
    "she", "or", "will", "my", "one", "all", "would", "there", "their", "what", "so", "if", "is",
    "was", "are", "were", "been", "has", "had", "can", "could", "should", "i", "another", "each",
    "every", "these", "those", "many", "several", "few", "both",
];

struct TrieNode {
    children: HashMap<char, TrieNode>,
    is_terminal: bool,
    frequency: u32,
}

impl TrieNode {
    fn new() -> Self {
        Self {
            children: HashMap::new(),
            is_terminal: false,
            frequency: 0,
        }
    }

    fn insert(&mut self, word: &str, freq: u32) {
        let mut node = self;
        for ch in word.chars() {
            node = node.children.entry(ch).or_insert_with(TrieNode::new);
        }
        node.is_terminal = true;
        node.frequency = node.frequency.max(freq);
    }

    fn find_prefix_node(&self, prefix: &str) -> Option<&TrieNode> {
        let mut node = self;
        for ch in prefix.chars() {
            node = node.children.get(&ch)?;
        }
        Some(node)
    }

    /// Collects terminal descendants as `(suffix, frequency)` pairs.
    fn collect_completions(&self, buf: &mut String, out: &mut Vec<(String, u32)>, cap: usize) {
        if out.len() >= cap {
            return;
        }

        if self.is_terminal && !buf.is_empty() {
            out.push((buf.clone(), self.frequency));
        }

        for (&ch, child) in &self.children {
            if out.len() >= cap {
                return;
            }
            buf.push(ch);
            child.collect_completions(buf, out, cap);
            buf.pop();
        }
    }
}

/// Builds the composite key used to index trigram successor maps.
fn trigram_key(w1: &str, w2: &str) -> String {
    let mut key = String::with_capacity(w1.len() + 1 + w2.len());
    key.push_str(w1);
    key.push('\t');
    key.push_str(w2);
    key
}

/// Returns a normalized key for a word used as prior n-gram context.
fn context_key(word: &str) -> Cow<'_, str> {
    if is_function_word(word) {
        Cow::Owned(word.to_ascii_lowercase())
    } else {
        Cow::Borrowed(word)
    }
}

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

/// Builds a normalized trigram context key.
fn normalized_trigram_key(w1: &str, w2: &str) -> String {
    let first = context_key(w1);
    let second = context_key(w2);
    trigram_key(first.as_ref(), second.as_ref())
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

#[derive(Serialize, Deserialize)]
struct VocabStore {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    words: HashMap<String, u32>,
    #[serde(default)]
    bigrams: HashMap<String, HashMap<String, u32>>,
    #[serde(default)]
    trigrams: HashMap<String, HashMap<String, u32>>,
}

impl Default for VocabStore {
    fn default() -> Self {
        Self {
            version: MODEL_VERSION,
            words: HashMap::new(),
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
        }
    }
}

/// Read access shared by the persistent vocabulary and local document index.
trait Lexicon {
    fn trie(&self) -> &TrieNode;
    fn bigram_successors(&self, word: &str) -> Option<&HashMap<String, u32>>;
    fn trigram_successors(&self, w1: &str, w2: &str) -> Option<&HashMap<String, u32>>;

    fn word_freq(&self, word: &str) -> u32 {
        let word = canonical_prediction_word(word);
        self.trie()
            .find_prefix_node(word)
            .filter(|node| node.is_terminal)
            .map(|node| node.frequency)
            .unwrap_or(0)
    }

    #[allow(dead_code)]
    fn bigram_count(&self, w1: &str, w2: &str) -> u32 {
        let successor = canonical_prediction_word(w2);
        self.bigram_successors(w1)
            .and_then(|map| map.get(successor))
            .copied()
            .unwrap_or(0)
    }

    #[allow(dead_code)]
    fn trigram_count(&self, w1: &str, w2: &str, w3: &str) -> u32 {
        let successor = canonical_prediction_word(w3);
        self.trigram_successors(w1, w2)
            .and_then(|map| map.get(successor))
            .copied()
            .unwrap_or(0)
    }
}

/// The persistent, cross-document vocabulary learned from typed text.
pub struct SharedVocabulary {
    store: VocabStore,
    trie: TrieNode,
    dictionary: Rc<Dictionary>,
    frozen: bool,
    dirty: bool,
}

impl Lexicon for SharedVocabulary {
    fn trie(&self) -> &TrieNode {
        &self.trie
    }

    fn bigram_successors(&self, word: &str) -> Option<&HashMap<String, u32>> {
        let key = context_key(word);
        self.store.bigrams.get(key.as_ref())
    }

    fn trigram_successors(&self, w1: &str, w2: &str) -> Option<&HashMap<String, u32>> {
        self.store.trigrams.get(&normalized_trigram_key(w1, w2))
    }
}

impl SharedVocabulary {
    /// Creates an empty vocabulary with no loaded dictionary.
    pub fn new() -> Self {
        Self {
            store: VocabStore::default(),
            trie: TrieNode::new(),
            dictionary: Rc::new(Dictionary::empty()),
            frozen: false,
            dirty: false,
        }
    }

    /// Loads the persisted vocabulary and migrates incompatible n-gram data.
    pub fn load() -> Self {
        let mut vocabulary = Self::new();
        vocabulary.dictionary = Rc::new(Dictionary::load_default());

        let Some(path) = Self::vocab_path() else {
            return vocabulary;
        };

        if !path.exists() {
            return vocabulary;
        }

        let Ok(data) = std::fs::read_to_string(&path) else {
            return vocabulary;
        };

        let Ok(mut store) = serde_json::from_str::<VocabStore>(&data) else {
            return vocabulary;
        };

        let migrated = store.version != MODEL_VERSION;
        if migrated {
            store.version = MODEL_VERSION;
            store.bigrams.clear();
            store.trigrams.clear();
        }

        for (word, &frequency) in &store.words {
            vocabulary.trie.insert(word, frequency);
        }

        vocabulary.store = store;
        vocabulary.dirty = migrated;

        if !migrated && let Ok(metadata) = std::fs::metadata(&path) {
            vocabulary.frozen = metadata.len() >= VOCAB_MAX_BYTES;
        }

        vocabulary
    }

    /// Returns the spelling dictionary shared with autocomplete hygiene.
    pub fn dictionary(&self) -> Rc<Dictionary> {
        self.dictionary.clone()
    }

    /// Persists the vocabulary if it has changed since the last save.
    pub fn save(&mut self) {
        if !self.dirty {
            return;
        }

        let Some(path) = Self::vocab_path() else {
            return;
        };

        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        if let Ok(data) = serde_json::to_string(&self.store) {
            if data.len() as u64 > VOCAB_MAX_BYTES {
                self.frozen = true;
                return;
            }

            if std::fs::write(&path, &data).is_ok() {
                self.dirty = false;
                self.frozen = false;
            }
        }
    }

    fn vocab_path() -> Option<PathBuf> {
        dirs::config_dir().map(|path| path.join("jot").join("vocabulary.json"))
    }

    fn learn_word(&mut self, word: &str) {
        if self.frozen {
            return;
        }

        let word = canonical_prediction_word(word);
        let entry = self.store.words.entry(word.to_string()).or_insert(0);
        *entry += 1;
        self.trie.insert(word, *entry);
        self.dirty = true;
    }

    fn learn_bigram(&mut self, w1: &str, w2: &str) {
        if self.frozen {
            return;
        }

        let first = context_key(w1).into_owned();
        let second = canonical_prediction_word(w2).to_string();

        *self
            .store
            .bigrams
            .entry(first)
            .or_default()
            .entry(second)
            .or_insert(0) += 1;

        self.dirty = true;
    }

    fn learn_trigram(&mut self, w1: &str, w2: &str, w3: &str) {
        if self.frozen {
            return;
        }

        let key = normalized_trigram_key(w1, w2);
        let successor = canonical_prediction_word(w3).to_string();

        *self
            .store
            .trigrams
            .entry(key)
            .or_default()
            .entry(successor)
            .or_insert(0) += 1;

        self.dirty = true;
    }

    /// Applies the prose trust gate before offering a word.
    fn is_trusted(&self, word: &str, shared_freq: u32, local_freq: u32) -> bool {
        if !self.dictionary.is_loaded() {
            return true;
        }

        self.dictionary.is_correct(word)
            || shared_freq >= NON_DICTIONARY_MIN_SHARED_FREQ
            || local_freq >= NON_DICTIONARY_MIN_LOCAL_FREQ
    }
}

/// Per-document index over the full current buffer.
struct WordIndex {
    root: TrieNode,
    bigrams: HashMap<String, HashMap<String, u32>>,
    trigrams: HashMap<String, HashMap<String, u32>>,
    last_hash: u64,
    last_rebuild: Option<Instant>,
}

impl Lexicon for WordIndex {
    fn trie(&self) -> &TrieNode {
        &self.root
    }

    fn bigram_successors(&self, word: &str) -> Option<&HashMap<String, u32>> {
        let key = context_key(word);
        self.bigrams.get(key.as_ref())
    }

    fn trigram_successors(&self, w1: &str, w2: &str) -> Option<&HashMap<String, u32>> {
        self.trigrams.get(&normalized_trigram_key(w1, w2))
    }
}

impl WordIndex {
    fn new() -> Self {
        Self {
            root: TrieNode::new(),
            bigrams: HashMap::new(),
            trigrams: HashMap::new(),
            last_hash: 0,
            last_rebuild: None,
        }
    }

    /// Rebuilds the local index when its throttle interval has elapsed.
    fn rebuild_if_stale(&mut self, text: &str, cursor: usize) {
        if let Some(last_rebuild) = self.last_rebuild
            && last_rebuild.elapsed() < LOCAL_REBUILD_INTERVAL
        {
            return;
        }

        self.last_rebuild = Some(Instant::now());

        let hash = hash_str(text);
        if hash == self.last_hash {
            return;
        }

        self.last_hash = hash;
        self.root = TrieNode::new();
        self.bigrams.clear();
        self.trigrams.clear();

        let sequences = extract_sentences_and_words(text, Some(cursor));
        let mut frequencies: HashMap<&str, u32> = HashMap::new();

        for sequence in &sequences {
            for &word in sequence {
                let word = canonical_prediction_word(word);
                *frequencies.entry(word).or_insert(0) += 1;
            }
        }

        for (word, frequency) in frequencies {
            self.root.insert(word, frequency);
        }

        for sequence in &sequences {
            for pair in sequence.windows(2) {
                let first = context_key(pair[0]).into_owned();
                let second = canonical_prediction_word(pair[1]).to_string();

                *self
                    .bigrams
                    .entry(first)
                    .or_default()
                    .entry(second)
                    .or_insert(0) += 1;
            }

            for triple in sequence.windows(3) {
                let key = normalized_trigram_key(triple[0], triple[1]);
                let successor = canonical_prediction_word(triple[2]).to_string();

                *self
                    .trigrams
                    .entry(key)
                    .or_default()
                    .entry(successor)
                    .or_insert(0) += 1;
            }
        }
    }
}

/// Classification of the text surrounding the cursor.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TextContext {
    Prose,
    Code,
}

/// Number preference supplied by a nearby determiner.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NumberPreference {
    Neutral,
    Singular,
    Plural,
}

/// Identity of a committed sentence occurrence already processed by a
/// completion provider.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct LearnedOccurrence {
    terminator_offset: usize,
    sentence_hash: u64,
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '-'
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
        if is_word_char(character) {
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

fn extract_prefix(text: &str, offset: usize) -> &str {
    if offset == 0 || offset > text.len() {
        return "";
    }

    let before = &text[..offset];
    let start = before
        .char_indices()
        .rev()
        .take_while(|&(_, character)| is_word_char(character))
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
        if is_word_char(character) {
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

/// Determines whether a word is a singular-oriented determiner.
fn is_singular_determiner(word: &str) -> bool {
    ["a", "an", "this", "that", "one", "another", "each", "every"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

/// Determines whether a word is a plural-oriented determiner.
fn is_plural_determiner(word: &str) -> bool {
    ["these", "those", "many", "several", "few", "both"]
        .iter()
        .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

/// Detects words that make a two-token determiner scope inference unsafe.
fn breaks_determiner_scope(word: &str) -> bool {
    [
        "of", "to", "for", "with", "in", "on", "by", "from", "and", "or", "but", "which", "who",
        "is", "are", "was", "were", "has", "have", "had",
    ]
    .iter()
    .any(|candidate| candidate.eq_ignore_ascii_case(word))
}

/// Infers a conservative number preference from the nearest two words.
fn number_preference(prev: Option<&str>, prev2: Option<&str>) -> NumberPreference {
    if let Some(word) = prev {
        if is_singular_determiner(word) {
            return NumberPreference::Singular;
        }

        if is_plural_determiner(word) {
            return NumberPreference::Plural;
        }
    }

    if let (Some(intervening), Some(determiner)) = (prev, prev2)
        && !breaks_determiner_scope(intervening)
    {
        if is_singular_determiner(determiner) {
            return NumberPreference::Singular;
        }

        if is_plural_determiner(determiner) {
            return NumberPreference::Plural;
        }
    }

    NumberPreference::Neutral
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

    for character in stripped.chars() {
        if character.is_whitespace() {
            continue;
        }

        nonspace += 1;

        if is_code_symbol(character) {
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

/// Lowercases the first ASCII character for sentence-start lookup.
fn lower_first(text: &str) -> String {
    let mut characters = text.chars();

    match characters.next() {
        Some(character) if character.is_ascii_uppercase() => {
            let mut result = String::with_capacity(text.len());
            result.push(character.to_ascii_lowercase());
            result.push_str(characters.as_str());
            result
        }
        _ => text.to_string(),
    }
}

/// A completion candidate merged across shared and local lexicons.
struct Candidate {
    suffix: String,
    shared: u32,
    local: u32,
}

/// Gathers completion candidates for a prefix from one lexicon.
fn add_candidates(
    lexicon: &impl Lexicon,
    lookup_prefix: &str,
    is_local: bool,
    output: &mut HashMap<String, Candidate>,
) {
    let Some(node) = lexicon.trie().find_prefix_node(lookup_prefix) else {
        return;
    };

    let mut buffer = String::new();
    let mut items = Vec::new();
    node.collect_completions(&mut buffer, &mut items, CANDIDATE_CAP);

    for (suffix, frequency) in items {
        if suffix.chars().count() < MIN_CANDIDATE_SUFFIX_LEN {
            continue;
        }

        let word = format!("{}{}", lookup_prefix, suffix);
        let entry = output.entry(word).or_insert_with(|| Candidate {
            suffix,
            shared: 0,
            local: 0,
        });

        if is_local {
            entry.local = entry.local.max(frequency);
        } else {
            entry.shared = entry.shared.max(frequency);
        }
    }
}

/// Merges successor distributions and returns the candidate count, total
/// support, and strongest successor count.
fn merged_candidate_stats(
    first: Option<&HashMap<String, u32>>,
    second: Option<&HashMap<String, u32>>,
    candidate: &str,
) -> (u32, u32, u32) {
    let candidate = canonical_prediction_word(candidate);
    let mut totals: HashMap<&str, u32> = HashMap::new();

    for map in [first, second].into_iter().flatten() {
        for (word, count) in map {
            *totals.entry(word.as_str()).or_insert(0) += *count;
        }
    }

    let candidate_count = totals.get(candidate).copied().unwrap_or(0);
    let total = totals.values().copied().sum();
    let strongest = totals.values().copied().max().unwrap_or(0);

    (candidate_count, total, strongest)
}

/// Converts successor statistics into confidence-weighted context evidence.
fn contextual_score(count: u32, total: u32, strongest: u32, weight: f64) -> f64 {
    if total == 0 {
        return 0.0;
    }

    let reliability = total as f64 / (total as f64 + CONTEXT_RELIABILITY_SUPPORT);

    if count > 0 {
        let probability = count as f64 / total as f64;
        return weight * reliability * (1.0 + count as f64).ln() * probability.sqrt();
    }

    if total < CONTEXT_MISS_MIN_TOTAL {
        return 0.0;
    }

    let concentration = strongest as f64 / total as f64;

    -weight * CONTEXT_MISS_PENALTY * reliability * concentration
}

/// Scores a candidate from unigram and confidence-weighted context evidence.
fn candidate_score(
    word: &str,
    candidate: &Candidate,
    prev: Option<&str>,
    prev2: Option<&str>,
    shared: &SharedVocabulary,
    local: &WordIndex,
) -> (f64, u32) {
    let unigram = candidate.shared as f64 + LOCAL_FREQ_WEIGHT * candidate.local as f64;

    let mut score = (1.0 + unigram).ln();
    let mut context_count = 0;

    if let Some(previous) = prev {
        let (count, total, strongest) = merged_candidate_stats(
            shared.bigram_successors(previous),
            local.bigram_successors(previous),
            word,
        );

        context_count += count;
        score += contextual_score(count, total, strongest, BIGRAM_WEIGHT);

        if let Some(previous_two) = prev2 {
            let (count, total, strongest) = merged_candidate_stats(
                shared.trigram_successors(previous_two, previous),
                local.trigram_successors(previous_two, previous),
                word,
            );

            context_count += count;
            score += contextual_score(count, total, strongest, TRIGRAM_WEIGHT);
        }
    }

    (score, context_count)
}

/// Determines whether the candidate map contains a singular form related to
/// a possible plural candidate.
fn has_singular_peer(word: &str, candidates: &HashMap<String, Candidate>) -> bool {
    word.strip_suffix("es")
        .is_some_and(|base| !base.is_empty() && candidates.contains_key(base))
        || word
            .strip_suffix('s')
            .is_some_and(|base| !base.is_empty() && candidates.contains_key(base))
}

/// Determines whether the candidate map contains a plural form related to a
/// possible singular candidate.
fn has_plural_peer(word: &str, candidates: &HashMap<String, Candidate>) -> bool {
    let with_s = format!("{}s", word);
    let with_es = format!("{}es", word);

    candidates.contains_key(&with_s) || candidates.contains_key(&with_es)
}

/// Applies a soft determiner-number adjustment when both inflectional forms
/// are competing candidates.
fn number_agreement_adjustment(
    word: &str,
    candidates: &HashMap<String, Candidate>,
    preference: NumberPreference,
) -> f64 {
    let has_singular = has_singular_peer(word, candidates);
    let has_plural = has_plural_peer(word, candidates);

    match preference {
        NumberPreference::Singular if has_plural => NUMBER_AGREEMENT_WEIGHT,
        NumberPreference::Singular if has_singular => -NUMBER_AGREEMENT_WEIGHT,
        NumberPreference::Plural if has_singular => NUMBER_AGREEMENT_WEIGHT,
        NumberPreference::Plural if has_plural => -NUMBER_AGREEMENT_WEIGHT,
        _ => 0.0,
    }
}

/// Returns whether a one-character suffix is useful enough to display.
fn should_display_one_character_suffix(candidate: &Candidate, context_count: u32) -> bool {
    if candidate.suffix.chars().count() != 1 {
        return true;
    }

    let combined_frequency =
        candidate.shared + candidate.local.saturating_mul(LOCAL_FREQ_WEIGHT as u32);

    context_count > 0 || combined_frequency >= ONE_CHAR_SUFFIX_MIN_FREQUENCY
}

/// Merges two successor maps and returns the best successor, its count, and
/// total distribution support.
fn merged_best(
    first: Option<&HashMap<String, u32>>,
    second: Option<&HashMap<String, u32>>,
) -> Option<(String, u32, u32)> {
    let mut totals: HashMap<&str, u32> = HashMap::new();

    for map in [first, second].into_iter().flatten() {
        for (word, count) in map {
            *totals.entry(word.as_str()).or_insert(0) += *count;
        }
    }

    if totals.is_empty() {
        return None;
    }

    let total = totals.values().copied().sum();

    let (word, count) = totals
        .into_iter()
        .max_by(|left, right| left.1.cmp(&right.1).then_with(|| right.0.cmp(left.0)))?;

    Some((word.to_string(), count, total))
}

/// Returns continuation statistics, preferring supported trigram evidence
/// and backing off to bigram evidence otherwise.
fn follow_stats(
    shared: &SharedVocabulary,
    local: &WordIndex,
    prior: Option<&str>,
    current: &str,
) -> Option<(String, u32, u32)> {
    if let Some(prior) = prior
        && let Some(stats) = merged_best(
            shared.trigram_successors(prior, current),
            local.trigram_successors(prior, current),
        )
        && stats.1 >= EXTEND_MIN_COUNT
    {
        return Some(stats);
    }

    merged_best(
        shared.bigram_successors(current),
        local.bigram_successors(current),
    )
}

fn valid_prediction(
    shared: &SharedVocabulary,
    local: &WordIndex,
    previous: &str,
    word: &str,
) -> bool {
    word != previous
        && is_prose_shaped(word)
        && shared.is_trusted(word, shared.word_freq(word), local.word_freq(word))
}

/// Predicts the next word after a completed word and a space.
fn predict_next_word(
    shared: &SharedVocabulary,
    local: &WordIndex,
    previous: &str,
    previous_two: Option<&str>,
) -> Option<String> {
    if let Some(previous_two) = previous_two
        && let Some((word, count, total)) = merged_best(
            shared.trigram_successors(previous_two, previous),
            local.trigram_successors(previous_two, previous),
        )
    {
        let probability = count as f64 / total as f64;

        if count >= PREDICT_TRIGRAM_MIN_COUNT
            && probability >= PREDICT_TRIGRAM_MIN_PROB
            && valid_prediction(shared, local, previous, &word)
        {
            return Some(word);
        }
    }

    let (word, count, total) = merged_best(
        shared.bigram_successors(previous),
        local.bigram_successors(previous),
    )?;

    let probability = count as f64 / total as f64;

    if count >= PREDICT_BIGRAM_MIN_COUNT
        && probability >= PREDICT_BIGRAM_MIN_PROB
        && valid_prediction(shared, local, previous, &word)
    {
        return Some(word);
    }

    None
}

/// Appends confidence-gated continuation words to a suggestion.
fn extend_suggestion(
    shared: &SharedVocabulary,
    local: &WordIndex,
    previous: Option<&str>,
    first: &str,
    output: &mut String,
    limit: usize,
) {
    let mut prior = previous.map(str::to_string);
    let mut last = first.to_string();
    let mut cumulative = 1.0;

    for step in 0..limit {
        let Some((word, count, total)) = follow_stats(shared, local, prior.as_deref(), &last)
        else {
            break;
        };

        if total == 0 {
            break;
        }

        let probability = count as f64 / total as f64;
        cumulative *= probability;

        let threshold = if step == 0 {
            EXTEND_FIRST_MIN_PROB
        } else {
            EXTEND_SECOND_MIN_PROB
        };

        if count < EXTEND_MIN_COUNT || probability < threshold || cumulative < EXTEND_MIN_CUMULATIVE
        {
            break;
        }

        if word == last || !is_prose_shaped(&word) {
            break;
        }

        if !shared.is_trusted(&word, shared.word_freq(&word), local.word_freq(&word)) {
            break;
        }

        output.push(' ');
        output.push_str(&word);
        prior = Some(std::mem::replace(&mut last, word));
    }
}

/// Generates an inline suggestion at the cursor.
fn generate_suggestion(
    shared: &SharedVocabulary,
    local: &WordIndex,
    text: &str,
    offset: usize,
) -> Option<String> {
    if has_text_after_cursor_on_line(text, offset) {
        return None;
    }

    let prefix = extract_prefix(text, offset);
    let prefix_start = offset - prefix.len();

    if has_closing_punctuation_before(text, prefix_start) {
        return None;
    }

    let context = classify_context(text, offset);
    let (previous, previous_two) = previous_two_words(text, prefix_start);
    let prefix_characters = prefix.chars().count();

    if prefix_characters == 0 {
        if context == TextContext::Code || !text[..offset].ends_with(' ') {
            return None;
        }

        let previous = previous?;
        let first = predict_next_word(shared, local, previous, previous_two)?;

        let mut result = first.clone();

        extend_suggestion(
            shared,
            local,
            Some(previous),
            &first,
            &mut result,
            EMPTY_PREDICTION_CONTINUATION_LIMIT,
        );

        return Some(result);
    }

    let mut candidates = HashMap::new();
    add_candidates(local, prefix, true, &mut candidates);

    if context == TextContext::Prose {
        add_candidates(shared, prefix, false, &mut candidates);

        let lowered = lower_first(prefix);
        if lowered != prefix {
            add_candidates(shared, &lowered, false, &mut candidates);
            add_candidates(local, &lowered, true, &mut candidates);
        }
    }

    let preference = number_preference(previous, previous_two);
    let mut scored = Vec::new();

    for (word, candidate) in &candidates {
        if context == TextContext::Prose {
            if !is_prose_shaped(word) {
                continue;
            }

            if !shared.is_trusted(word, candidate.shared, candidate.local) {
                continue;
            }
        }

        let (mut score, context_count) =
            candidate_score(word, candidate, previous, previous_two, shared, local);

        if context == TextContext::Prose {
            score += number_agreement_adjustment(word, &candidates, preference);
        }

        if prefix_characters < MIN_PREFIX_LEN && context_count < SHORT_PREFIX_MIN_CONTEXT {
            continue;
        }

        scored.push((score, context_count, word, candidate));
    }

    if scored.is_empty() {
        return None;
    }

    scored.sort_by(|left, right| {
        right
            .0
            .partial_cmp(&left.0)
            .unwrap_or(Ordering::Equal)
            .then_with(|| left.2.chars().count().cmp(&right.2.chars().count()))
            .then_with(|| left.2.cmp(right.2))
    });

    let best_score = scored[0].0;
    let best_context_count = scored[0].1;
    let best_word = scored[0].2.as_str();
    let best_candidate = scored[0].3;

    if !should_display_one_character_suffix(best_candidate, best_context_count) {
        return None;
    }

    let mut result = best_candidate.suffix.clone();

    if context == TextContext::Code {
        return Some(result);
    }

    let dominant = scored.len() < 2 || best_score >= scored[1].0 + std::f64::consts::LN_2;

    if dominant {
        extend_suggestion(
            shared,
            local,
            previous,
            best_word,
            &mut result,
            PREFIX_CONTINUATION_LIMIT,
        );
    }

    Some(result)
}

/// Inline completion provider backed by shared and local learned vocabulary.
pub struct JotCompletionProvider {
    shared_vocab: Rc<RefCell<SharedVocabulary>>,
    local_index: RefCell<WordIndex>,
    learned_occurrences: RefCell<HashSet<LearnedOccurrence>>,
    enabled: Rc<Cell<bool>>,
}

impl JotCompletionProvider {
    pub fn new(shared_vocab: Rc<RefCell<SharedVocabulary>>, enabled: Rc<Cell<bool>>) -> Self {
        Self {
            shared_vocab,
            local_index: RefCell::new(WordIndex::new()),
            learned_occurrences: RefCell::new(HashSet::new()),
            enabled,
        }
    }

    /// Learns the most recently committed prose sentence before the cursor.
    fn learn_preceding_sentence(&self, text: &str, offset: usize) {
        let search_floor =
            floor_char_boundary(text, offset.saturating_sub(TERMINATOR_SEARCH_BYTES));

        let search_window = &text[search_floor..offset];

        let Some(relative_terminator) =
            search_window.rfind(|character: char| is_sentence_terminator(character))
        else {
            return;
        };

        let terminator_offset = search_floor + relative_terminator;

        let inner_floor =
            floor_char_boundary(text, terminator_offset.saturating_sub(SENTENCE_SCAN_BYTES));

        let inner = &text[inner_floor..terminator_offset];

        let relative_start = inner
            .rfind(|character: char| is_sentence_terminator(character))
            .map(|index| index + 1)
            .unwrap_or(0);

        let sentence = inner[relative_start..].trim();

        if sentence.is_empty() {
            return;
        }

        let sentence_hash = hash_str(sentence);
        let occurrence = LearnedOccurrence {
            terminator_offset,
            sentence_hash,
        };

        if self.learned_occurrences.borrow().contains(&occurrence) {
            return;
        }

        if classify_context(text, terminator_offset) == TextContext::Code {
            return;
        }

        let sequences = extract_sentences_and_words(sentence, None);

        if sequences.is_empty() {
            return;
        }

        let mut learned_any = false;

        {
            let mut shared = self.shared_vocab.borrow_mut();

            for words in &sequences {
                for &word in words {
                    shared.learn_word(word);
                    learned_any = true;
                }

                for pair in words.windows(2) {
                    shared.learn_bigram(pair[0], pair[1]);
                }

                for triple in words.windows(3) {
                    shared.learn_trigram(triple[0], triple[1], triple[2]);
                }
            }
        }

        if learned_any {
            self.learned_occurrences.borrow_mut().insert(occurrence);
        }
    }
}

/// Offers one word, or a short run of words, to continue the text at the
/// caret. The text editor shows it as ghost text that Tab accepts.
impl SuggestionProvider for JotCompletionProvider {
    fn suggestions(
        &self,
        request: &SuggestionRequest,
        _window: &mut Window,
        _cx: &mut App,
    ) -> Task<anyhow::Result<Vec<Suggestion>>> {
        if !self.enabled.get() {
            return Task::ready(Ok(Vec::new()));
        }

        let text = request.text().to_string();
        let offset = floor_char_boundary(&text, request.offset().min(text.len()));

        self.learn_preceding_sentence(&text, offset);

        {
            let mut local = self.local_index.borrow_mut();
            local.rebuild_if_stale(&text, offset);
        }

        let suggestion = {
            let shared = self.shared_vocab.borrow();
            let local = self.local_index.borrow();

            generate_suggestion(&shared, &local, &text, offset)
        };

        Task::ready(Ok(suggestion.into_iter().map(Suggestion::new).collect()))
    }

    fn is_trigger(&self, _offset: usize, text: &str, _cx: &mut App) -> bool {
        if !self.enabled.get() {
            return false;
        }

        text.chars().any(|character| {
            character.is_alphanumeric()
                || character == '_'
                || character == '-'
                || character == ' '
                || is_sentence_terminator(character)
        })
    }
}

#[cfg(test)]
mod tests;
