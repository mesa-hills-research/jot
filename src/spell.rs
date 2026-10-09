//! Spell checking support for Jot.
//!
//! # Dictionary
//!
//! The [`Dictionary`] loads a plain wordlist (one word per line) from, in
//! order: the embedded asset `assets/dict/en.txt` (if present at build time),
//! `<config>/jot/dictionary.txt`, and `<config>/jot/user-dictionary.txt`.
//! Hunspell-style affix flags (`word/MS`) are tolerated and stripped. When no
//! adequately sized wordlist is available the dictionary reports itself as
//! not loaded and every correctness query answers permissively.
//!
//! Words may be added at runtime via [`Dictionary::add_word`], which also
//! appends them to the user dictionary file, or ignored until jot closes via
//! [`Dictionary::ignore_word`]. Both bump a generation counter so scanners
//! know to drop caches computed against the old dictionary.
//!
//! # Scanner
//!
//! [`SpellScanner`] finds the misspelled words of a whole document, designed
//! to run debounced on the UI thread:
//!
//! - Results are cached per line, keyed by a hash of the line's content, so
//!   an edit re-tokenizes only the changed line.
//! - Code is skipped using the shared code/prose line classifier from the
//!   autocomplete module, Markdown fence tracking, and neighborhood
//!   smoothing.
//! - Token rules follow a safe bias: identifiers, ALL-CAPS, camelCase,
//!   digit-adjacent tokens, paths, URLs, inline-code and tag-adjacent tokens
//!   are skipped rather than risk false squiggles.
//!
//! # Suggestion ranking
//!
//! Candidates are ranked by an edit distance weighted for typing mistakes:
//! doubling or undoubling a letter, swapping two neighbours (two vowels
//! least, as in "wierd") and confusing two vowels cost less than other edits,
//! so "mispeled" suggests "misspelled" first. Changing the first letter (typos rarely do) and offering a proper
//! noun for a lowercase word add to the cost, and a longer shared beginning
//! and a closer length break ties. The wordlist carries no word frequencies.
//!
//! # In the editor
//!
//! [`DocumentSpelling`] is the text editor's [`SpellChecker`]: the editor
//! decides when to check and draws the underlines and the fixes, and the
//! dictionary and scanner here decide what is misspelled and what to offer.

use crate::assets::Assets;
use crate::autocomplete::line_code_score;
use gpui_kit::component::input::{Rope, RopeExt, SpellCheck, SpellCheckRequest, SpellChecker};
use gpui_kit::{App, AssetSource, SharedString, Task};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

/// Idle time after the last edit before a document is re-checked.
pub const SPELL_CHECK_DEBOUNCE: Duration = Duration::from_millis(400);

/// Minimum number of entries for a wordlist to be considered a usable
/// dictionary. Guards against a lone user-dictionary flagging everything.
const MIN_USEFUL_DICTIONARY_SIZE: usize = 10_000;

/// Longest wordlist entry accepted at load time.
const MAX_DICTIONARY_WORD_LEN: usize = 48;

/// Most replacements offered for a misspelled word.
const MAX_SUGGESTIONS: usize = 5;

/// Largest weighted edit distance between a typo and a suggestion.
const MAX_SUGGESTION_COST: f32 = 2.0;

/// Largest difference in length, in characters, between a typo and a
/// suggestion.
const MAX_SUGGESTION_LENGTH_DIFFERENCE: usize = 3;

/// Cost of doubling or undoubling a letter: "untill", "ocured".
const DOUBLED_LETTER_COST: f32 = 0.4;

/// Cost of swapping two neighbouring letters: "teh", "hte".
const TRANSPOSITION_COST: f32 = 0.6;

/// Cost of swapping two neighbouring vowels, as in "i before e": "wierd",
/// "recieve".
const VOWEL_TRANSPOSITION_COST: f32 = 0.5;

/// Cost of writing one vowel for another: "seperate", "definately".
const VOWEL_COST: f32 = 0.75;

/// Cost of a missing or extra apostrophe: "dont".
const APOSTROPHE_COST: f32 = 0.5;

/// Added when a suggestion changes the first letter.
const FIRST_LETTER_COST: f32 = 0.5;

/// Added when a lowercase word would become a proper noun.
const CASE_COST: f32 = 0.5;

/// Cost of a suggestion that differs from the word only in case: "english".
const CASE_ONLY_COST: f32 = 0.3;

/// Documents larger than this are not spell checked.
const MAX_SPELL_DOCUMENT_BYTES: usize = 2 * 1024 * 1024;

/// Shortest token worth checking; also silences two-letter jargon (cx, px).
const MIN_CHECK_TOKEN_CHARS: usize = 3;

/// Longest token worth checking.
const MAX_CHECK_TOKEN_CHARS: usize = 48;

/// A line whose own code score exceeds this is never spell checked.
const LINE_CODE_SKIP_SCORE: f64 = 0.5;

/// A line whose neighborhood-smoothed code score exceeds this is skipped.
const SMOOTHED_CODE_SKIP_SCORE: f64 = 0.1;

/// An in-memory spelling dictionary backed by a plain wordlist.
///
/// Interior mutability allows runtime additions ("Add to dictionary") while
/// the dictionary is shared behind `Rc` across documents and providers.
pub struct Dictionary {
    words: RefCell<HashSet<Box<str>>>,
    /// Words accepted until jot closes, in lower case.
    ignored: RefCell<HashSet<Box<str>>>,
    loaded: bool,
    has_apostrophe_entries: Cell<bool>,
    generation: Cell<u64>,
}

impl Dictionary {
    /// Creates an empty, unloaded dictionary that answers permissively.
    pub fn empty() -> Self {
        Self {
            words: RefCell::new(HashSet::new()),
            ignored: RefCell::new(HashSet::new()),
            loaded: false,
            has_apostrophe_entries: Cell::new(false),
            generation: Cell::new(0),
        }
    }

    /// Loads the default dictionary from the embedded asset and any wordlist
    /// files found in the Jot config directory.
    pub fn load_default() -> Self {
        let mut wordlists = Vec::new();
        if let Ok(Some(data)) = Assets.load("dict/en.txt")
            && let Ok(content) = std::str::from_utf8(&data)
        {
            wordlists.push(content.to_string());
        }
        for path in Self::wordlist_paths() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                wordlists.push(content);
            }
        }
        Self::from_wordlists(wordlists.iter().map(String::as_str))
    }

    /// A dictionary of the words in `wordlists`.
    fn from_wordlists<'a>(wordlists: impl IntoIterator<Item = &'a str>) -> Self {
        let mut words: HashSet<Box<str>> = HashSet::new();
        for content in wordlists {
            Self::insert_wordlist(content, &mut words);
        }

        let loaded = words.len() >= MIN_USEFUL_DICTIONARY_SIZE;
        if loaded {
            log::info!("Spell dictionary loaded ({} words)", words.len());
        } else if words.is_empty() {
            log::info!("No spell dictionary found; spell checking is inactive");
        } else {
            log::warn!(
                "Wordlist too small to act as a dictionary ({} words); spell checking is inactive",
                words.len()
            );
        }

        let has_apostrophe_entries = words.iter().any(|w| w.contains('\''));

        Self {
            words: RefCell::new(words),
            ignored: RefCell::new(HashSet::new()),
            loaded,
            has_apostrophe_entries: Cell::new(has_apostrophe_entries),
            generation: Cell::new(0),
        }
    }

    /// Filesystem locations searched for wordlists, in load order.
    fn wordlist_paths() -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(dir) = dirs::config_dir() {
            let base = dir.join("jot");
            paths.push(base.join("dictionary.txt"));
            paths.push(base.join("user-dictionary.txt"));
        }
        paths
    }

    /// Location of the user's personal wordlist; the target of
    /// [`Dictionary::add_word`].
    pub fn user_dictionary_path() -> Option<PathBuf> {
        dirs::config_dir().map(|d| d.join("jot").join("user-dictionary.txt"))
    }

    /// Parses a wordlist, stripping Hunspell affix flags and skipping
    /// comments, blanks, and malformed entries.
    fn insert_wordlist(content: &str, into: &mut HashSet<Box<str>>) {
        for line in content.lines() {
            let entry = line.trim();
            if entry.is_empty() || entry.starts_with('#') {
                continue;
            }
            let word = entry.split('/').next().unwrap_or(entry).trim();
            if word.is_empty() || word.len() > MAX_DICTIONARY_WORD_LEN {
                continue;
            }
            if !word
                .chars()
                .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
            {
                continue;
            }
            into.insert(Box::from(word));
        }
    }

    /// Whether a usable dictionary was loaded.
    pub fn is_loaded(&self) -> bool {
        self.loaded
    }

    /// Number of entries in the dictionary.
    #[allow(dead_code)]
    pub fn word_count(&self) -> usize {
        self.words.borrow().len()
    }

    /// Monotonic counter incremented whenever the dictionary content
    /// changes, letting scanners invalidate caches lazily.
    pub fn generation(&self) -> u64 {
        self.generation.get()
    }

    /// Adds a word to the in-memory dictionary and appends it to the user
    /// dictionary file. Bumps the generation so cached scan results that
    /// flagged this word are discarded on the next scan.
    pub fn add_word(&self, word: &str) {
        let word = word.trim();
        if word.is_empty() || word.len() > MAX_DICTIONARY_WORD_LEN {
            return;
        }
        if !word
            .chars()
            .all(|c| c.is_alphabetic() || c == '\'' || c == '-')
        {
            return;
        }
        if !self.words.borrow_mut().insert(Box::from(word)) {
            return;
        }
        if word.contains('\'') {
            self.has_apostrophe_entries.set(true);
        }
        self.generation.set(self.generation.get() + 1);

        if let Some(path) = Self::user_dictionary_path() {
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Ok(mut file) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
            {
                use std::io::Write as _;
                let _ = writeln!(file, "{}", word);
            }
        }
    }

    /// Accepts `word`, in any case, until jot closes, without saving it.
    /// Bumps the generation like [`Dictionary::add_word`].
    pub fn ignore_word(&self, word: &str) {
        let word = word.trim().to_lowercase();
        if word.is_empty() {
            return;
        }
        if self.ignored.borrow_mut().insert(Box::from(word)) {
            self.generation.set(self.generation.get() + 1);
        }
    }

    /// Checks whether a word is spelled correctly.
    ///
    /// Returns `true` when no dictionary is loaded so callers degrade
    /// gracefully. Curly apostrophes are normalized; if the wordlist has no
    /// apostrophe entries at all, contractions are treated as unknowable and
    /// pass. Hyphenated compounds are validated part by part.
    pub fn is_correct(&self, word: &str) -> bool {
        if !self.loaded || word.is_empty() {
            return true;
        }
        {
            let ignored = self.ignored.borrow();
            if !ignored.is_empty() && ignored.contains(word.to_lowercase().as_str()) {
                return true;
            }
        }
        let normalized: Cow<'_, str> = if word.contains('\u{2019}') {
            Cow::Owned(word.replace('\u{2019}', "'"))
        } else {
            Cow::Borrowed(word)
        };
        let word = normalized.as_ref();
        if word.contains('\'') && !self.has_apostrophe_entries.get() {
            return true;
        }
        if word.contains('-') {
            return word
                .split('-')
                .all(|part| !part.is_empty() && self.lookup(part));
        }
        self.lookup(word)
    }

    /// Performs a case-relaxed dictionary lookup for a single token.
    fn lookup(&self, word: &str) -> bool {
        if word.chars().any(|c| c.is_ascii_digit() || c == '_') {
            return false;
        }
        let words = self.words.borrow();
        if words.contains(word) {
            return true;
        }
        let lower = word.to_lowercase();
        if lower != word && words.contains(lower.as_str()) {
            return true;
        }
        let is_all_caps = word.chars().all(|c| !c.is_lowercase());
        if is_all_caps && word.chars().count() > 1 {
            let title = capitalize(&lower);
            if words.contains(title.as_str()) {
                return true;
            }
        }
        false
    }

    /// Generates up to `limit` correction suggestions for a misspelled word,
    /// best first. See the module docs for the ranking.
    pub fn suggest(&self, word: &str, limit: usize) -> Vec<String> {
        if !self.loaded || word.is_empty() || limit == 0 {
            return Vec::new();
        }
        let target: Vec<char> = word.to_lowercase().chars().collect();
        if target.is_empty() || target.len() > MAX_DICTIONARY_WORD_LEN {
            return Vec::new();
        }
        let first = target.first().copied();
        let last = target.last().copied();
        let target_is_lowercase = word.chars().all(|c| !c.is_uppercase());

        let words = self.words.borrow();
        let mut matches: Vec<(f32, &str)> = Vec::new();
        for cand in words.iter() {
            let cand_len = cand.chars().count();
            if cand_len.abs_diff(target.len()) > MAX_SUGGESTION_LENGTH_DIFFERENCE {
                continue;
            }
            let cand_first = cand.chars().next().map(|c| c.to_ascii_lowercase());
            let cand_last = cand.chars().last().map(|c| c.to_ascii_lowercase());
            if cand_first != first && cand_last != last {
                continue;
            }
            let cand_chars: Vec<char> = cand.to_lowercase().chars().collect();
            let Some(mut cost) = typo_distance(&target, &cand_chars, MAX_SUGGESTION_COST) else {
                continue;
            };
            if cost == 0.0 {
                if cand.as_ref() == word {
                    continue;
                }
                cost = CASE_ONLY_COST;
            }
            if cand_first != first {
                cost += FIRST_LETTER_COST;
            }
            if target_is_lowercase && cand.chars().any(|c| c.is_uppercase()) {
                cost += CASE_COST;
            }
            // Tie-breaks, each smaller than any edit: a longer shared
            // beginning and a closer length.
            let shared_prefix = target
                .iter()
                .zip(&cand_chars)
                .take_while(|(a, b)| a == b)
                .count()
                .min(4);
            cost -= 0.02 * shared_prefix as f32;
            cost += 0.03 * cand_len.abs_diff(target.len()) as f32;
            matches.push((cost, cand.as_ref()));
        }

        matches.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        let mut suggestions: Vec<String> = Vec::with_capacity(limit);
        for (_, candidate) in matches {
            let suggestion = match_case(word, candidate);
            if !suggestions.contains(&suggestion) {
                suggestions.push(suggestion);
            }
            if suggestions.len() == limit {
                break;
            }
        }
        suggestions
    }
}

/// A document's spell checking in the text editor.
///
/// Each document has its own, so the scanner's line cache follows that
/// document, and they share the dictionary.
pub struct DocumentSpelling {
    dictionary: Rc<Dictionary>,
    scanner: RefCell<SpellScanner>,
}

impl DocumentSpelling {
    pub fn new(dictionary: Rc<Dictionary>) -> Self {
        Self {
            dictionary,
            scanner: RefCell::new(SpellScanner::new()),
        }
    }
}

impl SpellChecker for DocumentSpelling {
    /// Checks the whole document whatever lines were edited: whether a line is
    /// code depends on its neighbours and on fences above it, and the
    /// scanner's cache keeps unchanged lines cheap.
    fn check(&self, request: &SpellCheckRequest, _: &mut App) -> Task<anyhow::Result<SpellCheck>> {
        let text = request.text();
        let misspelled = self.scanner.borrow_mut().scan(&self.dictionary, text);
        Task::ready(Ok(SpellCheck {
            checked: std::iter::once(0..text.len()).collect(),
            misspelled,
        }))
    }

    fn suggestions(&self, word: &str, _: &mut App) -> Vec<SharedString> {
        self.dictionary
            .suggest(word, MAX_SUGGESTIONS)
            .into_iter()
            .map(SharedString::from)
            .collect()
    }

    fn add_to_dictionary(&self, word: &str, _: &mut App) {
        self.dictionary.add_word(word);
    }

    /// Ignored words stay ignored in every document until jot closes.
    fn ignore(&self, word: &str, _: &mut App) {
        self.dictionary.ignore_word(word);
    }

    fn debounce(&self) -> Duration {
        SPELL_CHECK_DEBOUNCE
    }
}

/// Cached analysis for one unique line content.
struct CachedLine {
    code_score: f64,
    is_fence: bool,
    /// Byte ranges of the misspelled words within the line.
    misspellings: Option<Rc<Vec<Range<usize>>>>,
}

/// Per-row working record for one scan pass.
struct LineRecord {
    hash: u64,
    code_score: f64,
    is_fence: bool,
    content: String,
}

/// Incremental, cache-backed document spell scanner. One instance per
/// document so caches track that document's content.
pub struct SpellScanner {
    cache: HashMap<u64, CachedLine>,
    dictionary_generation: u64,
}

impl SpellScanner {
    pub fn new() -> Self {
        Self {
            cache: HashMap::new(),
            dictionary_generation: 0,
        }
    }

    /// Scans the document and returns the byte ranges of all misspellings
    /// outside code regions, in order.
    pub fn scan(&mut self, dictionary: &Dictionary, text: &Rope) -> Vec<Range<usize>> {
        if !dictionary.is_loaded() || text.len() == 0 || text.len() > MAX_SPELL_DOCUMENT_BYTES {
            self.cache.clear();
            return Vec::new();
        }

        if dictionary.generation() != self.dictionary_generation {
            self.cache.clear();
            self.dictionary_generation = dictionary.generation();
        }

        let mut rows: Vec<LineRecord> = Vec::with_capacity(text.lines_len());
        let mut new_cache: HashMap<u64, CachedLine> = HashMap::with_capacity(text.lines_len());

        for line in text.iter_lines() {
            let content = line.to_string();
            let hash = hash_line(&content);
            if let std::collections::hash_map::Entry::Vacant(e) = new_cache.entry(hash) {
                let entry = self.cache.remove(&hash).unwrap_or_else(|| CachedLine {
                    code_score: line_code_score(&content),
                    is_fence: content.trim_start().starts_with("```"),
                    misspellings: None,
                });
                e.insert(entry);
            }
            let entry = &new_cache[&hash];
            rows.push(LineRecord {
                hash,
                code_score: entry.code_score,
                is_fence: entry.is_fence,
                content,
            });
        }

        let mut issues = Vec::new();
        let mut in_fence = false;

        for (row, record) in rows.iter().enumerate() {
            if record.is_fence {
                in_fence = !in_fence;
                continue;
            }
            if in_fence || record.code_score > LINE_CODE_SKIP_SCORE {
                continue;
            }
            if smoothed_code_score(&rows, row) > SMOOTHED_CODE_SKIP_SCORE {
                continue;
            }

            let entry = new_cache
                .get_mut(&record.hash)
                .expect("entry inserted in first pass");
            let misspellings = match &entry.misspellings {
                Some(m) => m.clone(),
                None => {
                    let computed = Rc::new(check_line(dictionary, &record.content));
                    entry.misspellings = Some(computed.clone());
                    computed
                }
            };

            let line_start = text.line_start_offset(row);
            issues.extend(
                misspellings
                    .iter()
                    .map(|word| line_start + word.start..line_start + word.end),
            );
        }

        self.cache = new_cache;
        issues
    }
}

/// Weighted average of code scores over a five-line neighborhood, so lines
/// inside code blocks inherit their surroundings' classification.
fn smoothed_code_score(rows: &[LineRecord], row: usize) -> f64 {
    const WEIGHTS: [(isize, f64); 5] = [(-2, 1.0), (-1, 2.0), (0, 4.0), (1, 2.0), (2, 1.0)];
    let mut total = 0.0;
    let mut weight = 0.0;
    for (delta, w) in WEIGHTS {
        let Some(ix) = row.checked_add_signed(delta) else {
            continue;
        };
        let Some(record) = rows.get(ix) else {
            continue;
        };
        total += w * record.code_score;
        weight += w;
    }
    if weight == 0.0 { 0.0 } else { total / weight }
}

fn hash_line(line: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    line.hash(&mut hasher);
    hasher.finish()
}

fn is_token_char(c: char) -> bool {
    c.is_alphabetic() || c == '\'' || c == '\u{2019}' || c == '-'
}

/// Tokenizes one line and returns the byte ranges of its misspellings.
fn check_line(dictionary: &Dictionary, line: &str) -> Vec<Range<usize>> {
    let mut result = Vec::new();
    if line.len() < MIN_CHECK_TOKEN_CHARS {
        return result;
    }
    let chars: Vec<(usize, char)> = line.char_indices().collect();
    let mut ix = 0;
    while ix < chars.len() {
        if !is_token_char(chars[ix].1) {
            ix += 1;
            continue;
        }
        let raw_start = ix;
        while ix < chars.len() && is_token_char(chars[ix].1) {
            ix += 1;
        }
        examine_token(dictionary, &chars, raw_start, ix, &mut result);
    }
    result
}

/// Applies shape, adjacency, and dictionary checks to a single raw token.
///
/// Every skip rule errs toward silence: a missed check is a soft failure,
/// while a false squiggle on jargon or code is the failure mode that
/// actually annoys.
fn examine_token(
    dictionary: &Dictionary,
    chars: &[(usize, char)],
    raw_start: usize,
    raw_end: usize,
    out: &mut Vec<Range<usize>>,
) {
    let mut start = raw_start;
    let mut end = raw_end;
    while start < end && !chars[start].1.is_alphabetic() {
        start += 1;
    }
    while end > start && !chars[end - 1].1.is_alphabetic() {
        end -= 1;
    }
    let len = end - start;
    if !(MIN_CHECK_TOKEN_CHARS..=MAX_CHECK_TOKEN_CHARS).contains(&len) {
        return;
    }

    let mut has_lower = false;
    let mut has_upper = false;
    let mut internal_upper = false;
    for (i, &(_, c)) in chars[start..end].iter().enumerate() {
        if !c.is_ascii() && c != '\u{2019}' {
            return;
        }
        if c.is_ascii_uppercase() {
            has_upper = true;
            if i > 0 {
                internal_upper = true;
            }
        } else if c.is_ascii_lowercase() {
            has_lower = true;
        }
    }
    if has_upper && !has_lower {
        return;
    }
    if internal_upper {
        return;
    }

    let prev = (raw_start > 0).then(|| chars[raw_start - 1].1);
    let next = chars.get(raw_end).map(|&(_, c)| c);
    let next2 = chars.get(raw_end + 1).map(|&(_, c)| c);

    if let Some(p) = prev
        && (p.is_ascii_digit()
            || matches!(
                p,
                '_' | '.' | '@' | '/' | '\\' | '#' | '$' | '&' | '=' | '~' | '`' | '<' | ':'
            ))
    {
        return;
    }
    if let Some(n) = next {
        if n.is_ascii_digit() || matches!(n, '_' | '@' | '(' | '=' | '`' | '>' | '/' | '\\') {
            return;
        }
        if n == '.' && next2.is_some_and(|c| c.is_alphanumeric()) {
            return;
        }
        if n == ':' && next2.is_some_and(|c| !c.is_whitespace()) {
            return;
        }
    }

    let word: String = chars[start..end].iter().map(|&(_, c)| c).collect();
    if dictionary.is_correct(&word) {
        return;
    }
    if let Some(base) = word
        .strip_suffix("'s")
        .or_else(|| word.strip_suffix("\u{2019}s"))
        && dictionary.is_correct(base)
    {
        return;
    }
    if has_upper && !starts_sentence(chars, raw_start) {
        return;
    }

    let end_byte = chars.get(end).map_or_else(
        || chars[end - 1].0 + chars[end - 1].1.len_utf8(),
        |&(offset, _)| offset,
    );
    out.push(chars[start].0..end_byte);
}

/// Determines whether a token sits at a sentence start, scanning back over
/// whitespace and opening quotes/brackets. Capitalized unknown words that do
/// not start a sentence are treated as proper nouns and skipped.
fn starts_sentence(chars: &[(usize, char)], raw_start: usize) -> bool {
    let mut ix = raw_start;
    while ix > 0 {
        let c = chars[ix - 1].1;
        if c.is_whitespace()
            || matches!(
                c,
                '"' | '\'' | '(' | '[' | '{' | '\u{2018}' | '\u{201C}' | '*'
            )
        {
            ix -= 1;
            continue;
        }
        return matches!(c, '.' | '!' | '?' | ':');
    }
    true
}

fn is_vowel(c: char) -> bool {
    matches!(c, 'a' | 'e' | 'i' | 'o' | 'u')
}

/// Cost of the character at `ix` (1-based) of `word` being missing from the
/// other word: cheap when it doubles the letter before it, or is an
/// apostrophe.
fn gap_cost(word: &[char], ix: usize) -> f32 {
    let c = word[ix - 1];
    if c == '\'' || c == '\u{2019}' {
        APOSTROPHE_COST
    } else if ix >= 2 && word[ix - 2] == c {
        DOUBLED_LETTER_COST
    } else {
        1.0
    }
}

fn substitution_cost(a: char, b: char) -> f32 {
    if a == b {
        0.0
    } else if is_vowel(a) && is_vowel(b) {
        VOWEL_COST
    } else {
        1.0
    }
}

/// Optimal string alignment (restricted Damerau-Levenshtein) distance with
/// costs weighted for typing mistakes, from typed `a` to candidate `b`.
/// Returns `None` as soon as the distance provably exceeds `max`.
fn typo_distance(a: &[char], b: &[char], max: f32) -> Option<f32> {
    let n = b.len();
    let mut prev2: Vec<f32> = vec![0.0; n + 1];
    let mut prev: Vec<f32> = vec![0.0; n + 1];
    for j in 1..=n {
        prev[j] = prev[j - 1] + gap_cost(b, j);
    }
    let mut cur: Vec<f32> = vec![0.0; n + 1];

    for i in 1..=a.len() {
        cur[0] = prev[0] + gap_cost(a, i);
        let mut row_min = cur[0];
        for j in 1..=n {
            let mut v = (prev[j] + gap_cost(a, i))
                .min(cur[j - 1] + gap_cost(b, j))
                .min(prev[j - 1] + substitution_cost(a[i - 1], b[j - 1]));
            if i > 1
                && j > 1
                && a[i - 1] != a[i - 2]
                && a[i - 1] == b[j - 2]
                && a[i - 2] == b[j - 1]
            {
                let cost = if is_vowel(a[i - 1]) && is_vowel(a[i - 2]) {
                    VOWEL_TRANSPOSITION_COST
                } else {
                    TRANSPOSITION_COST
                };
                v = v.min(prev2[j - 2] + cost);
            }
            cur[j] = v;
            row_min = row_min.min(v);
        }
        if row_min > max {
            return None;
        }
        std::mem::swap(&mut prev2, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }

    let d = prev[n];
    (d <= max).then_some(d)
}

/// Adjusts a suggestion's casing to match the shape of the source word.
fn match_case(source: &str, suggestion: &str) -> String {
    match source.chars().next() {
        Some(f) if f.is_uppercase() => {
            let all_caps = source.chars().all(|c| !c.is_lowercase()) && source.chars().count() > 1;
            if all_caps {
                suggestion.to_uppercase()
            } else {
                capitalize(suggestion)
            }
        }
        _ => suggestion.to_string(),
    }
}

/// Uppercases the first character of a string.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dictionary() -> Dictionary {
        let words =
            std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/dict/en.txt"))
                .expect("the embedded wordlist");
        let dictionary = Dictionary::from_wordlists([words.as_str()]);
        assert!(dictionary.is_loaded());
        dictionary
    }

    #[test]
    fn common_typos_suggest_the_intended_word_first() {
        let dictionary = dictionary();
        for (typo, intended) in [
            ("mispeled", "misspelled"),
            ("teh", "the"),
            ("recieve", "receive"),
            ("seperate", "separate"),
            ("occured", "occurred"),
            ("definately", "definitely"),
            ("accomodate", "accommodate"),
            ("untill", "until"),
            ("wierd", "weird"),
            ("becuase", "because"),
            ("adress", "address"),
            ("tommorow", "tomorrow"),
            ("Teh", "The"),
        ] {
            let suggestions = dictionary.suggest(typo, MAX_SUGGESTIONS);
            assert_eq!(
                suggestions.first().map(String::as_str),
                Some(intended),
                "{typo}: {suggestions:?}"
            );
        }
    }

    #[test]
    fn doubled_letters_and_swaps_cost_less_than_other_edits() {
        let chars = |s: &str| s.chars().collect::<Vec<_>>();
        let cost = |a: &str, b: &str| typo_distance(&chars(a), &chars(b), 3.0).unwrap();
        assert_eq!(cost("untill", "until"), DOUBLED_LETTER_COST);
        assert_eq!(cost("teh", "the"), TRANSPOSITION_COST);
        assert_eq!(cost("wierd", "weird"), VOWEL_TRANSPOSITION_COST);
        assert_eq!(cost("seperate", "separate"), VOWEL_COST);
        assert_eq!(cost("cat", "cut"), VOWEL_COST);
        assert_eq!(cost("cat", "cot"), VOWEL_COST);
        assert_eq!(cost("cat", "bat"), 1.0);
        assert_eq!(cost("dont", "don't"), APOSTROPHE_COST);
        assert!(typo_distance(&chars("abc"), &chars("xyz"), 2.0).is_none());
    }

    #[test]
    fn misspellings_are_byte_ranges_of_the_document() {
        let dictionary = dictionary();
        let mut scanner = SpellScanner::new();
        let text = Rope::from("Café notes.\nThis line is mispeled here.\n");
        let found = scanner.scan(&dictionary, &text);
        let words: Vec<String> = found
            .iter()
            .map(|range| text.slice(range.clone()).to_string())
            .collect();
        assert_eq!(words, ["mispeled"]);
    }
}
