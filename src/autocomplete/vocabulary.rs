//! What autocomplete knows: words, and the words that follow one or two
//! others, each with how often it was seen.
//!
//! [`SharedVocabulary`] holds what the user typed across documents and saves
//! it to `vocabulary.json`. [`LocalIndex`] counts the words of the open
//! document, for words and identifiers the vocabulary hasn't learned.
//!
//! # Case
//!
//! Words are kept case-folded: "Thanks" opening a sentence and "thanks"
//! inside one are one word, followed by the same words, and the typographic
//! apostrophe folds to the plain one. Each word keeps the forms it was
//! written in, with their counts, so a suggestion shows "iPhone" as typed,
//! and a word predicted after another shows the form it took after that one.
//!
//! # File
//!
//! `vocabulary.json` holds a `version`, `words` (each form with its count),
//! `bigrams` (each folded word with the forms that followed it) and
//! `trigrams` (the same for two folded words joined by a tab). `<s>` stands
//! for the start of a sentence. Files from before keys were folded load with
//! the keys that differ only in case merged and their counts summed, and the
//! halves of contractions they split ("doesn" from "doesn't") joined again.
//! Files without a version keep their word counts and drop their n-grams,
//! which an older tokenizer produced.

use super::{
    LOCAL_COUNT_CAP, MODEL_VERSION, NON_DICTIONARY_MIN_LOCAL_FREQ, NON_DICTIONARY_MIN_SHARED_FREQ,
    VOCAB_MAX_BYTES, canonical_prediction_word, extract_sentences_and_words, hash_str,
    sentence_runs,
};
use crate::spell::Dictionary;
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// The context that stands for the start of a sentence. No word is written
/// this way.
pub(super) const SENTENCE_START: &str = "<s>";

/// Minimum interval between full rebuilds of the local document index.
const LOCAL_REBUILD_INTERVAL: Duration = Duration::from_secs(2);

/// Halves of contractions that earlier versions learned as words of their
/// own, which only ever come from "n't". "don" also stands alone in its
/// lowercase form.
const CONTRACTION_HALVES: &[&str] = &[
    "ain", "aren", "couldn", "didn", "doesn", "hadn", "hasn", "isn", "mightn", "mustn", "needn",
    "shan", "shouldn", "wasn", "weren", "wouldn",
];

/// Folds a word's case, and the typographic apostrophe to the plain one.
pub(super) fn fold(word: &str) -> Cow<'_, str> {
    let folded = |character: char| {
        character != '\u{2019}' && character.to_lowercase().eq(std::iter::once(character))
    };
    if word.chars().all(folded) {
        return Cow::Borrowed(word);
    }
    Cow::Owned(
        word.chars()
            .flat_map(char::to_lowercase)
            .map(|character| {
                if character == '\u{2019}' {
                    '\''
                } else {
                    character
                }
            })
            .collect(),
    )
}

/// A word, as saved by a version that split contractions, with its halves
/// joined: "doesn" becomes "doesn't".
fn joined(word: &str) -> Cow<'_, str> {
    let lower = fold(word);
    if CONTRACTION_HALVES.contains(&lower.as_ref()) || word == "don" {
        Cow::Owned(format!("{word}'t"))
    } else {
        Cow::Borrowed(word)
    }
}

/// How often a word was seen, and in which forms.
#[derive(Clone, Debug, Default)]
pub(super) struct Forms {
    count: u32,
    /// Forms other than the folded word itself, with their counts.
    cased: Vec<(Box<str>, u32)>,
}

impl Forms {
    pub fn count(&self) -> u32 {
        self.count
    }

    fn add(&mut self, form: &str, folded: &str, count: u32) {
        self.count = self.count.saturating_add(count);
        if form == folded {
            return;
        }
        match self.cased.iter_mut().find(|(cased, _)| &**cased == form) {
            Some((_, cased_count)) => *cased_count = cased_count.saturating_add(count),
            None => self.cased.push((form.into(), count)),
        }
    }

    /// The form the word was most often written in. A tie goes to the folded
    /// form, then to the form seen first.
    pub fn best<'a>(&'a self, folded: &'a str) -> &'a str {
        let mut best = (folded, self.plain());
        for (form, count) in &self.cased {
            if *count > best.1 {
                best = (form, *count);
            }
        }
        best.0
    }

    /// The count of the folded form itself.
    fn plain(&self) -> u32 {
        self.count - self.cased.iter().map(|(_, count)| count).sum::<u32>()
    }

    /// Each form with its count, as saved.
    fn each<'a>(&'a self, folded: &'a str) -> impl Iterator<Item = (&'a str, u32)> {
        let plain = self.plain();
        (plain > 0)
            .then_some((folded, plain))
            .into_iter()
            .chain(self.cased.iter().map(|(form, count)| (&**form, *count)))
    }
}

/// The words that followed one context, by folded form, with counts.
#[derive(Debug, Default)]
pub(super) struct Successors {
    total: u32,
    counts: HashMap<Box<str>, u32>,
    /// Forms other than the folded word, with their counts.
    cased: HashMap<Box<str>, u32>,
}

impl Successors {
    /// How many times anything followed this context.
    pub fn total(&self) -> u32 {
        self.total
    }

    /// How many different words followed it.
    pub fn types(&self) -> usize {
        self.counts.len()
    }

    /// How many times the folded word followed it.
    pub fn count(&self, folded: &str) -> u32 {
        self.counts.get(folded).copied().unwrap_or(0)
    }

    pub fn words(&self) -> impl Iterator<Item = &str> {
        self.counts.keys().map(|word| &**word)
    }

    fn add(&mut self, form: &str, folded: &str, count: u32) {
        self.total = self.total.saturating_add(count);
        let entry = self.counts.entry(folded.into()).or_default();
        *entry = entry.saturating_add(count);
        if form != folded {
            let entry = self.cased.entry(form.into()).or_default();
            *entry = entry.saturating_add(count);
        }
    }

    /// The form the folded word most often took after this context.
    pub fn form<'a>(&'a self, folded: &'a str) -> &'a str {
        let mut plain = self.count(folded);
        let mut best: Option<(&str, u32)> = None;
        for (form, &count) in &self.cased {
            if fold(form) == folded {
                plain = plain.saturating_sub(count);
                if best.is_none_or(|(_, best_count)| count > best_count) {
                    best = Some((form, count));
                }
            }
        }
        match best {
            Some((form, count)) if count > plain => form,
            _ => folded,
        }
    }
}

/// Words and n-grams with their counts.
#[derive(Default)]
pub(super) struct Counts {
    /// Every word by folded form, in order, for looking words up by prefix.
    words: BTreeMap<Box<str>, Forms>,
    /// The sum of the word counts.
    total: u64,
    /// What followed each folded word, or the start of a sentence.
    bigrams: HashMap<Box<str>, Successors>,
    /// What followed each pair of folded words, joined by a tab.
    trigrams: HashMap<Box<str>, Successors>,
}

impl Counts {
    pub fn word(&self, folded: &str) -> Option<&Forms> {
        self.words.get(folded)
    }

    pub fn word_count(&self, folded: &str) -> u32 {
        self.word(folded).map_or(0, Forms::count)
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    /// The words that start with `folded_prefix`, in order.
    pub fn words_with_prefix<'a>(
        &'a self,
        folded_prefix: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a Forms)> {
        self.words
            .range::<str, _>((
                std::ops::Bound::Included(folded_prefix),
                std::ops::Bound::Unbounded,
            ))
            .take_while(move |(word, _)| word.starts_with(folded_prefix))
            .map(|(word, forms)| (&**word, forms))
    }

    /// What followed `context`: one folded word, or two.
    pub fn successors(&self, context: &[String]) -> Option<&Successors> {
        match context {
            [word] => self.bigrams.get(word.as_str()),
            [first, second] => self.trigrams.get(trigram_key(first, second).as_str()),
            _ => None,
        }
    }

    fn add_word(&mut self, form: &str, count: u32) {
        let folded = fold(form);
        self.words
            .entry(folded.as_ref().into())
            .or_default()
            .add(form, &folded, count);
        self.total += u64::from(count);
    }

    /// Counts a run of words in one sentence. One that opens the sentence is
    /// counted after the sentence start too.
    pub fn add_run(&mut self, starts_sentence: bool, run: &[&str]) {
        let forms: Vec<&str> = run
            .iter()
            .map(|word| canonical_prediction_word(word))
            .collect();
        let folded: Vec<Cow<'_, str>> = forms.iter().map(|form| fold(form)).collect();
        for form in &forms {
            self.add_word(form, 1);
        }
        let mut sequence: Vec<&str> = Vec::with_capacity(run.len() + 1);
        if starts_sentence {
            sequence.push(SENTENCE_START);
        }
        let offset = sequence.len();
        sequence.extend(folded.iter().map(|word| word.as_ref()));
        for (index, form) in forms.iter().enumerate() {
            let position = index + offset;
            if position >= 1 {
                self.bigrams
                    .entry(sequence[position - 1].into())
                    .or_default()
                    .add(form, &folded[index], 1);
            }
            if position >= 2 {
                self.trigrams
                    .entry(trigram_key(sequence[position - 2], sequence[position - 1]).into())
                    .or_default()
                    .add(form, &folded[index], 1);
            }
        }
    }

    /// Counts loaded from a file: keys folded and contraction halves joined.
    fn from_saved(saved: SavedVocabulary) -> Self {
        let mut counts = Self::default();
        for (form, count) in saved.words {
            counts.add_word(&joined(&form), count);
        }
        for (key, successors) in saved.bigrams {
            let key: Box<str> = fold(&joined(&key)).as_ref().into();
            let entry = counts.bigrams.entry(key).or_default();
            for (form, count) in successors {
                let form = joined(&form);
                entry.add(&form, &fold(&form), count);
            }
        }
        for (key, successors) in saved.trigrams {
            let Some((first, second)) = key.split_once('\t') else {
                continue;
            };
            let key = trigram_key(&fold(&joined(first)), &fold(&joined(second)));
            let entry = counts.trigrams.entry(key.into()).or_default();
            for (form, count) in successors {
                let form = joined(&form);
                entry.add(&form, &fold(&form), count);
            }
        }
        counts
    }
}

/// The key of the trigram context `first second`.
fn trigram_key(first: &str, second: &str) -> String {
    let mut key = String::with_capacity(first.len() + 1 + second.len());
    key.push_str(first);
    key.push('\t');
    key.push_str(second);
    key
}

/// `vocabulary.json` as read.
#[derive(Default, Deserialize)]
struct SavedVocabulary {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    words: HashMap<String, u32>,
    #[serde(default)]
    bigrams: HashMap<String, HashMap<String, u32>>,
    #[serde(default)]
    trigrams: HashMap<String, HashMap<String, u32>>,
}

/// `vocabulary.json` as written, straight from the counts.
struct Saving<'a>(&'a Counts);

impl Serialize for Saving<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("version", &MODEL_VERSION)?;
        map.serialize_entry("words", &SavingWords(&self.0.words))?;
        map.serialize_entry("bigrams", &SavingNgrams(&self.0.bigrams))?;
        map.serialize_entry("trigrams", &SavingNgrams(&self.0.trigrams))?;
        map.end()
    }
}

struct SavingWords<'a>(&'a BTreeMap<Box<str>, Forms>);

impl Serialize for SavingWords<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (folded, forms) in self.0 {
            for (form, count) in forms.each(folded) {
                map.serialize_entry(form, &count)?;
            }
        }
        map.end()
    }
}

struct SavingNgrams<'a>(&'a HashMap<Box<str>, Successors>);

impl Serialize for SavingNgrams<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, successors) in self.0 {
            map.serialize_entry(&**key, &SavingSuccessors(successors))?;
        }
        map.end()
    }
}

struct SavingSuccessors<'a>(&'a Successors);

impl Serialize for SavingSuccessors<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let successors = self.0;
        let mut cased_counts: HashMap<Cow<'_, str>, u32> = HashMap::new();
        for (form, &count) in &successors.cased {
            *cased_counts.entry(fold(form)).or_default() += count;
        }
        let mut map = serializer.serialize_map(None)?;
        for (folded, &count) in &successors.counts {
            let plain = count.saturating_sub(cased_counts.get(&**folded).copied().unwrap_or(0));
            if plain > 0 {
                map.serialize_entry(&**folded, &plain)?;
            }
        }
        for (form, count) in &successors.cased {
            map.serialize_entry(&**form, count)?;
        }
        map.end()
    }
}

/// The persistent, cross-document vocabulary learned from typed text.
pub struct SharedVocabulary {
    counts: Counts,
    dictionary: Rc<Dictionary>,
    dirty: bool,
}

impl SharedVocabulary {
    /// Creates an empty vocabulary with no loaded dictionary.
    pub fn new() -> Self {
        Self {
            counts: Counts::default(),
            dictionary: Rc::new(Dictionary::empty()),
            dirty: false,
        }
    }

    /// Creates an empty vocabulary that trusts the words of `dictionary`.
    pub fn with_dictionary(dictionary: Rc<Dictionary>) -> Self {
        Self {
            dictionary,
            ..Self::new()
        }
    }

    /// Loads the persisted vocabulary, with the spelling dictionary.
    pub fn load() -> Self {
        let mut vocabulary = Self::with_dictionary(Rc::new(Dictionary::load_default()));

        let Some(path) = Self::vocab_path() else {
            return vocabulary;
        };

        let Ok(data) = std::fs::read(&path) else {
            return vocabulary;
        };

        let Ok(saved) = serde_json::from_slice::<SavedVocabulary>(&data) else {
            return vocabulary;
        };

        vocabulary.read(saved);
        vocabulary
    }

    /// Takes the counts of a file. One saved before version 2 keeps its
    /// words and drops its n-grams.
    fn read(&mut self, mut saved: SavedVocabulary) {
        let migrated = saved.version < MODEL_VERSION;
        if migrated {
            saved.bigrams.clear();
            saved.trigrams.clear();
        }
        self.counts = Counts::from_saved(saved);
        self.dirty = migrated;
    }

    /// The vocabulary as `vocabulary.json` holds it.
    fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        serde_json::to_vec(&Saving(&self.counts))
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

        if let Ok(data) = self.to_json()
            && data.len() as u64 <= VOCAB_MAX_BYTES
            && std::fs::write(&path, &data).is_ok()
        {
            self.dirty = false;
        }
    }

    fn vocab_path() -> Option<PathBuf> {
        dirs::config_dir().map(|path| path.join("jot").join("vocabulary.json"))
    }

    pub(super) fn counts(&self) -> &Counts {
        &self.counts
    }

    /// Learns the words of one sentence and what follows what. Returns the
    /// folded words it learned.
    pub(super) fn learn_sentence(&mut self, sentence: &str) -> Vec<String> {
        let mut learned = Vec::new();
        for (starts_sentence, run) in sentence_runs(sentence) {
            self.counts.add_run(starts_sentence, &run);
            learned.extend(
                run.iter()
                    .map(|word| fold(canonical_prediction_word(word)).into_owned()),
            );
            self.dirty = true;
        }
        learned
    }

    /// Whether `word` may be offered in prose: a dictionary word, or one the
    /// user wrote often enough not to be a one-off typo.
    pub(super) fn is_trusted(&self, word: &str, shared_count: u32, local_count: u32) -> bool {
        !self.dictionary.is_loaded()
            || self.dictionary.is_correct(word)
            || shared_count >= NON_DICTIONARY_MIN_SHARED_FREQ
            || local_count >= NON_DICTIONARY_MIN_LOCAL_FREQ
    }

    /// Each word form with its count.
    #[cfg(test)]
    pub(super) fn word_counts(&self) -> Vec<(String, u32)> {
        let mut words: Vec<(String, u32)> = self
            .counts
            .words
            .iter()
            .flat_map(|(folded, forms)| forms.each(folded))
            .map(|(form, count)| (form.to_string(), count))
            .collect();
        words.sort();
        words
    }

    /// Counts `word` as seen once more.
    #[cfg(test)]
    pub(super) fn learn_word(&mut self, word: &str) {
        self.counts.add_word(word, 1);
    }

    /// Counts `second` as having followed `first` once more.
    #[cfg(test)]
    pub(super) fn learn_bigram(&mut self, first: &str, second: &str) {
        self.counts
            .bigrams
            .entry(fold(first).as_ref().into())
            .or_default()
            .add(second, &fold(second), 1);
    }
}

/// The words of the open document, rebuilt from its text now and then.
///
/// In code they are all the suggestions draw on. In prose they add the
/// document's own words to the shared vocabulary, such as names in a pasted
/// email. A word's count there is its occurrences in the document that the
/// vocabulary can't already hold: occurrences this document taught it don't
/// count, and neither do occurrences the document opened with, up to the
/// vocabulary's own count of the word, since an earlier session may have
/// learned them. That way no text counts twice.
pub(super) struct LocalIndex {
    counts: Counts,
    /// Per folded word, occurrences the shared vocabulary doesn't hold.
    extra: HashMap<Box<str>, u32>,
    extra_total: u64,
    last_hash: u64,
    last_rebuild: Option<Instant>,
    stale: bool,
}

/// What a document added to the shared vocabulary, or opened with: folded
/// words with counts.
pub(super) type WordTally = HashMap<Box<str>, u32>;

impl LocalIndex {
    pub fn new() -> Self {
        Self {
            counts: Counts::default(),
            extra: HashMap::new(),
            extra_total: 0,
            last_hash: 0,
            last_rebuild: None,
            stale: true,
        }
    }

    /// The document's own counts, every occurrence included.
    pub fn counts(&self) -> &Counts {
        &self.counts
    }

    /// Occurrences of a folded word that the shared vocabulary doesn't hold.
    pub fn extra(&self, folded: &str) -> u32 {
        self.extra.get(folded).copied().unwrap_or(0)
    }

    pub fn extra_total(&self) -> u64 {
        self.extra_total
    }

    /// Rebuilds at the next chance, however recent the last rebuild.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Rebuilds the index from `text` when its throttle interval has passed
    /// or it was marked stale. The word at `cursor` is being typed and isn't
    /// counted.
    pub fn rebuild_if_stale(
        &mut self,
        text: &str,
        cursor: usize,
        now: Instant,
        shared: &Counts,
        taught: &WordTally,
        opened: &WordTally,
    ) {
        if !self.stale
            && let Some(last_rebuild) = self.last_rebuild
            && now.saturating_duration_since(last_rebuild) < LOCAL_REBUILD_INTERVAL
        {
            return;
        }
        self.last_rebuild = Some(now);

        let hash = hash_str(text);
        if hash == self.last_hash && !self.stale {
            return;
        }
        self.last_hash = hash;
        self.stale = false;

        self.counts = Counts::default();
        for run in extract_sentences_and_words(text, Some(cursor)) {
            self.counts.add_run(false, &run);
        }

        self.extra.clear();
        self.extra_total = 0;
        for (folded, forms) in &self.counts.words {
            let held = taught.get(folded).copied().unwrap_or(0)
                + opened
                    .get(folded)
                    .copied()
                    .unwrap_or(0)
                    .min(shared.word_count(folded));
            let extra = forms.count().saturating_sub(held).min(LOCAL_COUNT_CAP);
            if extra > 0 {
                self.extra.insert(folded.clone(), extra);
                self.extra_total += u64::from(extra);
            }
        }
    }
}

/// Folded learnable words of `text`, counted.
pub(super) fn tally(text: &str) -> WordTally {
    let mut tally = WordTally::new();
    for run in extract_sentences_and_words(text, None) {
        for word in run {
            *tally
                .entry(fold(canonical_prediction_word(word)).as_ref().into())
                .or_default() += 1;
        }
    }
    tally
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(json: &str) -> SharedVocabulary {
        let mut vocabulary = SharedVocabulary::new();
        vocabulary.read(serde_json::from_str(json).unwrap());
        vocabulary
    }

    fn bigram(vocabulary: &SharedVocabulary, first: &str, second: &str) -> u32 {
        vocabulary
            .counts
            .successors(&[first.to_string()])
            .map_or(0, |successors| successors.count(second))
    }

    fn trigram(vocabulary: &SharedVocabulary, first: &str, second: &str, third: &str) -> u32 {
        vocabulary
            .counts
            .successors(&[first.to_string(), second.to_string()])
            .map_or(0, |successors| successors.count(third))
    }

    #[test]
    fn folding_lowercases_and_straightens_apostrophes() {
        assert_eq!(fold("thanks"), "thanks");
        assert!(matches!(fold("thanks"), Cow::Borrowed(_)));
        assert_eq!(fold("Thanks"), "thanks");
        assert_eq!(fold("We’ll"), "we'll");
        assert_eq!(fold("ÉCOLE"), "école");
    }

    /// A file saved before keys were folded, with keys that differ only in
    /// case and contractions split in two, loads with its counts merged and
    /// none lost.
    #[test]
    fn an_earlier_vocabulary_loads_with_its_counts_merged() {
        let vocabulary = load(
            r#"{"version":2,
                "words":{"Thanks":4,"thanks":3,"for":7,"the":6,"It":2,"it":3,"doesn":5,"Doesn":1,"don":2,"Don":4,"can":2,"I":3},
                "bigrams":{"Thanks":{"for":4},"thanks":{"for":3},"it":{"doesn":3},"It":{"doesn":2},"I":{"don":2,"can":1},"<s>":{"Don":4}},
                "trigrams":{"Thanks\tfor":{"the":4},"thanks\tfor":{"the":2},"I\tcan":{"See":1,"see":2}}}"#,
        );
        assert!(!vocabulary.dirty);
        let words = vocabulary.word_counts();
        let count = |word: &str| {
            words
                .iter()
                .find(|(form, _)| form == word)
                .map_or(0, |(_, count)| *count)
        };
        assert_eq!((count("Thanks"), count("thanks")), (4, 3));
        assert_eq!(vocabulary.counts.word_count("thanks"), 7);
        assert_eq!(vocabulary.counts.word_count("it"), 5);
        assert_eq!(bigram(&vocabulary, "thanks", "for"), 7);
        assert_eq!(trigram(&vocabulary, "thanks", "for", "the"), 6);
        assert_eq!(trigram(&vocabulary, "i", "can", "see"), 3);

        // The halves of "doesn't" and "don't" join, the name "Don" and the
        // word "can" stay.
        assert_eq!(
            (count("doesn't"), count("Doesn't"), count("doesn")),
            (5, 1, 0)
        );
        assert_eq!((count("don't"), count("Don"), count("don")), (2, 4, 0));
        assert_eq!(count("can"), 2);
        assert_eq!(bigram(&vocabulary, "it", "doesn't"), 5);
        assert_eq!(bigram(&vocabulary, "i", "don't"), 2);
        assert_eq!(
            vocabulary.counts.total(),
            4 + 3 + 7 + 6 + 2 + 3 + 5 + 1 + 2 + 4 + 2 + 3
        );

        // Saved and loaded again, it is the same.
        let saved = String::from_utf8(vocabulary.to_json().unwrap()).unwrap();
        let reloaded = load(&saved);
        assert_eq!(reloaded.word_counts(), vocabulary.word_counts());
        assert_eq!(bigram(&reloaded, "thanks", "for"), 7);
        assert_eq!(trigram(&reloaded, "i", "can", "see"), 3);
        assert_eq!(
            reloaded
                .counts
                .successors(&["i".into(), "can".into()])
                .unwrap()
                .form("see"),
            "see"
        );
    }

    /// A file without a version keeps its word counts and drops its n-grams,
    /// which an earlier tokenizer made, and is saved again as version 2.
    #[test]
    fn a_file_without_a_version_keeps_only_its_words() {
        let vocabulary = load(
            r#"{"words":{"Thanks":40,"for":90},"bigrams":{"Thanks":{"for":38}},"trigrams":{"Thanks\tfor":{"the":20}}}"#,
        );
        assert!(vocabulary.dirty);
        assert_eq!(vocabulary.counts.word_count("thanks"), 40);
        assert_eq!(bigram(&vocabulary, "thanks", "for"), 0);
        assert_eq!(trigram(&vocabulary, "thanks", "for", "the"), 0);
        let saved: serde_json::Value =
            serde_json::from_slice(&vocabulary.to_json().unwrap()).unwrap();
        assert_eq!(saved["version"], 2);
        assert_eq!(saved["words"]["Thanks"], 40);
        assert_eq!(saved["bigrams"], serde_json::json!({}));
    }

    /// A word predicted after another shows the form it took after that one.
    #[test]
    fn successors_keep_their_forms() {
        let mut counts = Counts::default();
        counts.add_run(true, &["Thanks", "for", "the", "US", "visit"]);
        counts.add_run(false, &["for", "the", "US", "trip"]);
        counts.add_run(false, &["the", "us"]);
        let after_the = counts.successors(&["the".into()]).unwrap();
        assert_eq!((after_the.count("us"), after_the.form("us")), (3, "US"));
        let start = counts.successors(&[SENTENCE_START.into()]).unwrap();
        assert_eq!((start.count("thanks"), start.form("thanks")), (1, "Thanks"));
        assert_eq!(counts.word("thanks").unwrap().best("thanks"), "Thanks");
        assert_eq!(counts.word("us").unwrap().best("us"), "US");
    }
}
