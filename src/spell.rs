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
//! appends them to the user dictionary file and bumps a generation counter
//! so scanners know to drop caches computed against the old dictionary.
//!
//! # Scanner
//!
//! [`SpellScanner`] produces [`SpellIssue`]s for a whole document, designed
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
//! - Suggestions are computed once per unique line content and rationed per
//!   scan so large pastes cannot stall the UI.
//!
//! # Suggestion ranking
//!
//! Candidates are ranked by edit distance, then first-letter preservation
//! (typos rarely change the first letter), then case-class agreement (a
//! lowercase typo prefers lowercase corrections over proper nouns), then
//! length proximity to the typed word, then alphabetically.

use crate::assets::Assets;
use crate::autocomplete::line_code_score;
use gpui_kit::AssetSource;
use gpui_kit::component::input::{Position, Rope, RopeExt};
use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
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

/// Maximum edit distance considered when generating correction suggestions.
const MAX_EDIT_DISTANCE: usize = 2;

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

/// Maximum number of correction suggestions attached to one misspelling.
const MAX_SUGGESTIONS: usize = 3;

/// Maximum number of fresh suggestion computations per scan.
const SUGGESTION_BUDGET_PER_SCAN: usize = 40;

/// An in-memory spelling dictionary backed by a plain wordlist.
///
/// Interior mutability allows runtime additions ("Add to dictionary") while
/// the dictionary is shared behind `Rc` across documents and providers.
pub struct Dictionary {
    words: RefCell<HashSet<Box<str>>>,
    loaded: bool,
    has_apostrophe_entries: Cell<bool>,
    generation: Cell<u64>,
}

impl Dictionary {
    /// Creates an empty, unloaded dictionary that answers permissively.
    pub fn empty() -> Self {
        Self {
            words: RefCell::new(HashSet::new()),
            loaded: false,
            has_apostrophe_entries: Cell::new(false),
            generation: Cell::new(0),
        }
    }

    /// Loads the default dictionary from the embedded asset and any wordlist
    /// files found in the Jot config directory.
    pub fn load_default() -> Self {
        let mut words: HashSet<Box<str>> = HashSet::new();

        if let Ok(Some(data)) = Assets.load("dict/en.txt") {
            if let Ok(content) = std::str::from_utf8(&data) {
                Self::insert_wordlist(content, &mut words);
            }
        }

        for path in Self::wordlist_paths() {
            if let Ok(content) = std::fs::read_to_string(&path) {
                Self::insert_wordlist(&content, &mut words);
            }
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

    /// Generates up to `limit` correction suggestions for a misspelled word.
    ///
    /// Ranking key, in priority order: edit distance, first-letter
    /// preservation, case-class agreement, length proximity, alphabetical.
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
        let mut matches: Vec<((usize, u8, u8, usize), &str)> = Vec::new();
        for cand in words.iter() {
            let cand_len = cand.chars().count();
            let len_diff = cand_len.abs_diff(target.len());
            if len_diff > MAX_EDIT_DISTANCE {
                continue;
            }
            let cand_first = cand.chars().next().map(|c| c.to_ascii_lowercase());
            let cand_last = cand.chars().last().map(|c| c.to_ascii_lowercase());
            if cand_first != first && cand_last != last {
                continue;
            }
            let cand_chars: Vec<char> = cand.to_lowercase().chars().collect();
            let Some(d) = bounded_osa_distance(&target, &cand_chars, MAX_EDIT_DISTANCE) else {
                continue;
            };
            if d == 0 {
                continue;
            }
            let first_mismatch = (cand_first != first) as u8;
            let case_mismatch =
                (target_is_lowercase && cand.chars().any(|c| c.is_uppercase())) as u8;
            matches.push(((d, first_mismatch, case_mismatch, len_diff), cand.as_ref()));
        }

        matches.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(b.1)));
        matches.truncate(limit);
        matches
            .into_iter()
            .map(|(_, w)| match_case(word, w))
            .collect()
    }
}

/// A misspelling located in a document, in editor [`Position`] coordinates
/// (0-based line, 0-based character column).
#[derive(Clone)]
pub struct SpellIssue {
    pub line: u32,
    pub start_character: u32,
    pub end_character: u32,
    pub word: String,
    pub suggestions: Vec<String>,
}

/// A misspelling within a single line, cached by line content.
#[derive(Clone)]
struct Misspelling {
    start_character: u32,
    end_character: u32,
    word: String,
    suggestions: Vec<String>,
}

/// Cached analysis for one unique line content.
struct CachedLine {
    code_score: f64,
    is_fence: bool,
    misspellings: Option<Rc<Vec<Misspelling>>>,
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

    /// Scans the document and returns all misspellings outside code regions.
    ///
    /// The token containing `cursor` (if any) is suppressed so the word
    /// currently being typed never flickers a squiggle.
    pub fn scan(
        &mut self,
        dictionary: &Dictionary,
        text: &Rope,
        cursor: Option<Position>,
    ) -> Vec<SpellIssue> {
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
            if !new_cache.contains_key(&hash) {
                let entry = self.cache.remove(&hash).unwrap_or_else(|| CachedLine {
                    code_score: line_code_score(&content),
                    is_fence: content.trim_start().starts_with("```"),
                    misspellings: None,
                });
                new_cache.insert(hash, entry);
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
        let mut suggestion_budget = SUGGESTION_BUDGET_PER_SCAN;
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
                    let computed = Rc::new(check_line(
                        dictionary,
                        &record.content,
                        &mut suggestion_budget,
                    ));
                    entry.misspellings = Some(computed.clone());
                    computed
                }
            };

            for m in misspellings.iter() {
                let at_cursor = cursor.is_some_and(|c| {
                    c.line as usize == row
                        && c.character >= m.start_character
                        && c.character <= m.end_character
                });
                if at_cursor {
                    continue;
                }
                issues.push(SpellIssue {
                    line: row as u32,
                    start_character: m.start_character,
                    end_character: m.end_character,
                    word: m.word.clone(),
                    suggestions: m.suggestions.clone(),
                });
            }
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

/// Tokenizes one line and returns its misspellings with char-column spans.
fn check_line(
    dictionary: &Dictionary,
    line: &str,
    suggestion_budget: &mut usize,
) -> Vec<Misspelling> {
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
        examine_token(dictionary, &chars, raw_start, ix, suggestion_budget, &mut result);
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
    suggestion_budget: &mut usize,
    out: &mut Vec<Misspelling>,
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
    if len < MIN_CHECK_TOKEN_CHARS || len > MAX_CHECK_TOKEN_CHARS {
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

    if let Some(p) = prev {
        if p.is_ascii_digit()
            || matches!(
                p,
                '_' | '.' | '@' | '/' | '\\' | '#' | '$' | '&' | '=' | '~' | '`' | '<' | ':'
            )
        {
            return;
        }
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
    {
        if dictionary.is_correct(base) {
            return;
        }
    }
    if has_upper && !starts_sentence(chars, raw_start) {
        return;
    }

    let suggestions = if *suggestion_budget > 0 {
        *suggestion_budget -= 1;
        dictionary.suggest(&word, MAX_SUGGESTIONS)
    } else {
        Vec::new()
    };

    out.push(Misspelling {
        start_character: start as u32,
        end_character: end as u32,
        word,
        suggestions,
    });
}

/// Determines whether a token sits at a sentence start, scanning back over
/// whitespace and opening quotes/brackets. Capitalized unknown words that do
/// not start a sentence are treated as proper nouns and skipped.
fn starts_sentence(chars: &[(usize, char)], raw_start: usize) -> bool {
    let mut ix = raw_start;
    while ix > 0 {
        let c = chars[ix - 1].1;
        if c.is_whitespace()
            || matches!(c, '"' | '\'' | '(' | '[' | '{' | '\u{2018}' | '\u{201C}' | '*')
        {
            ix -= 1;
            continue;
        }
        return matches!(c, '.' | '!' | '?' | ':');
    }
    true
}

/// Optimal string alignment (restricted Damerau-Levenshtein) distance with a
/// cutoff. Returns `None` as soon as the distance provably exceeds `max`.
fn bounded_osa_distance(a: &[char], b: &[char], max: usize) -> Option<usize> {
    if a.len().abs_diff(b.len()) > max {
        return None;
    }
    let n = b.len();
    let mut prev2: Vec<usize> = vec![0; n + 1];
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut cur: Vec<usize> = vec![0; n + 1];

    for i in 1..=a.len() {
        cur[0] = i;
        let mut row_min = cur[0];
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            let mut v = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(prev2[j - 2] + 1);
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
    if d <= max { Some(d) } else { None }
}

/// Adjusts a suggestion's casing to match the shape of the source word.
fn match_case(source: &str, suggestion: &str) -> String {
    match source.chars().next() {
        Some(f) if f.is_uppercase() => {
            let all_caps =
                source.chars().all(|c| !c.is_lowercase()) && source.chars().count() > 1;
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