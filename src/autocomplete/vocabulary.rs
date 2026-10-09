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
//!
//! # Saving
//!
//! jot saves the vocabulary when a file is saved, a tab or the window closes,
//! and jot quits, and every few minutes while it has learned something new.
//! The file is written whole beside the old one, which it then replaces, so
//! a crash leaves one or the other. Near its size cap the vocabulary drops
//! the entries it has seen least, and goes on learning. A file that doesn't
//! parse is kept aside rather than written over.
//!
//! Saves run on a background thread, so typing never waits for one
//! ([`SharedVocabulary::save_in_background`]). Quitting and closing the
//! window save on the UI thread, since the save has to finish
//! ([`SharedVocabulary::save`]), after waiting for a background save under
//! way. Learning while a background save runs stays for the next save.
//!
//! # Several jots at once
//!
//! Each running jot loads the file once, at startup, and learns on its own.
//! A save adds what this jot learned since its last save to what the file
//! holds now, so every jot's learning adds up, whatever the order of saves.
//! A save takes the lock on `vocabulary.lock` beside the file, reads the
//! file, adds its counts, writes the file whole and goes on from the result.
//! So another jot's learning reaches this one when this one next saves. A
//! save that can't get the lock within [`LOCK_WAIT`] keeps its learning for
//! the next one.
//!
//! Loading takes no lock, since a save replaces the file in one step. A
//! crash loses only what that jot learned since its last save. A file gone
//! or damaged by the time of a save gives way to this jot's own vocabulary,
//! learning included, and a damaged one is kept aside as at loading.
//!
//! Pruning applies to the file's counts with the new learning added. An
//! entry pruned from the file comes back at the next save of a jot that has
//! seen it since, counted from what that jot saw since its last save.

use super::{
    LOCAL_COUNT_CAP, MODEL_VERSION, NON_DICTIONARY_MIN_LOCAL_FREQ, NON_DICTIONARY_MIN_SHARED_FREQ,
    VOCAB_MAX_BYTES, canonical_prediction_word, extract_sentences_and_words, hash_str,
    sentence_runs,
};
use crate::spell::Dictionary;
use gpui_kit::{App, BackgroundExecutor, Task};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::fs::{File, TryLockError};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// The context that stands for the start of a sentence. No word is written
/// this way.
pub(super) const SENTENCE_START: &str = "<s>";

/// Minimum interval between full rebuilds of the local document index.
const LOCAL_REBUILD_INTERVAL: Duration = Duration::from_secs(2);

/// How long a save waits for another jot to finish saving. Saving a
/// vocabulary at its size cap takes a second or two.
const LOCK_WAIT: Duration = Duration::from_secs(5);

/// How often a save waiting for the lock tries it again.
const LOCK_RETRY: Duration = Duration::from_millis(20);

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

    /// Adds the counts of `other`, the same word's.
    fn add_all(&mut self, other: &Forms) {
        self.count = self.count.saturating_add(other.count);
        for (form, count) in &other.cased {
            match self.cased.iter_mut().find(|(cased, _)| cased == form) {
                Some((_, cased_count)) => *cased_count = cased_count.saturating_add(*count),
                None => self.cased.push((form.clone(), *count)),
            }
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
#[derive(Clone, Debug, Default)]
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

    /// Adds the counts of `other`, the same context's.
    fn add_all(&mut self, other: &Successors) {
        self.total = self.total.saturating_add(other.total);
        for (counts, others) in [
            (&mut self.counts, &other.counts),
            (&mut self.cased, &other.cased),
        ] {
            for (word, count) in others {
                let entry = counts.entry(word.clone()).or_default();
                *entry = entry.saturating_add(*count);
            }
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
#[derive(Clone, Default)]
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

    /// Adds the counts of `other`.
    fn add(&mut self, other: &Counts) {
        for (word, forms) in &other.words {
            self.words.entry(word.clone()).or_default().add_all(forms);
        }
        self.total += other.total;
        for (ngrams, others) in [
            (&mut self.bigrams, &other.bigrams),
            (&mut self.trigrams, &other.trigrams),
        ] {
            for (context, successors) in others {
                ngrams
                    .entry(context.clone())
                    .or_default()
                    .add_all(successors);
            }
        }
    }

    /// Drops the entries of `level` seen `floor` times or fewer.
    fn prune(&mut self, level: Level, floor: u32) {
        match level {
            Level::Words => {
                self.words.retain(|_, forms| forms.count > floor);
                self.total = self
                    .words
                    .values()
                    .map(|forms| u64::from(forms.count))
                    .sum();
            }
            Level::Bigrams => prune_successors(&mut self.bigrams, floor),
            Level::Trigrams => prune_successors(&mut self.trigrams, floor),
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

/// The kinds of entries in [`Counts`].
#[derive(Clone, Copy)]
enum Level {
    Words,
    Bigrams,
    Trigrams,
}

/// Drops the successors seen `floor` times or fewer, and the contexts left
/// with none.
fn prune_successors(map: &mut HashMap<Box<str>, Successors>, floor: u32) {
    map.retain(|_, successors| {
        successors.counts.retain(|_, count| *count > floor);
        let counts = &successors.counts;
        successors
            .cased
            .retain(|form, _| counts.contains_key(fold(form).as_ref()));
        successors.total = counts.values().sum();
        !counts.is_empty()
    });
}

/// Moves a file that couldn't be read to an unused name beside it:
/// `vocabulary.damaged.json`, then `vocabulary.damaged-2.json` and on.
fn set_aside(path: &Path) -> io::Result<PathBuf> {
    let stem = path.file_stem().unwrap_or_default().to_string_lossy();
    for number in 1.. {
        let name = match number {
            1 => format!("{stem}.damaged.json"),
            number => format!("{stem}.damaged-{number}.json"),
        };
        let kept = path.with_file_name(name);
        if !kept.exists() {
            std::fs::rename(path, &kept)?;
            return Ok(kept);
        }
    }
    unreachable!()
}

/// Writes `data` to `path` whole: to a temporary file beside it first, which
/// then takes its place.
fn write_whole(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(format!(".{}.tmp", std::process::id()));
    let temporary = PathBuf::from(temporary);
    let written = (|| {
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(data)?;
        file.sync_all()?;
        drop(file);
        replace(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written
}

/// Renames `from` over `to`. On Windows, as elsewhere, the rename replaces a
/// file that exists, even one another jot has open as it loads, since the
/// standard library opens files with delete sharing. Another program, such
/// as a virus scanner, can hold the file open without it for a moment, so a
/// refusal is tried again a few times.
fn replace(from: &Path, to: &Path) -> io::Result<()> {
    let mut tries = 0;
    loop {
        match std::fs::rename(from, to) {
            Err(error)
                if cfg!(windows)
                    && tries < 5
                    && error.kind() == io::ErrorKind::PermissionDenied =>
            {
                tries += 1;
                std::thread::sleep(Duration::from_millis(50));
            }
            result => return result,
        }
    }
}

/// The lock file beside the vocabulary at `path`: `vocabulary.lock`. It stays
/// there between saves.
fn lock_path(path: &Path) -> PathBuf {
    path.with_extension("lock")
}

/// Takes the lock on the vocabulary at `path`, waiting up to `wait` while
/// another jot holds it. The lock is released when the returned file is
/// dropped or the process ends, however it ends.
fn lock(path: &Path, wait: Duration) -> io::Result<File> {
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path(path))?;
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(LOCK_RETRY);
            }
            Err(TryLockError::WouldBlock) => {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("another jot was saving it for over {} ms", wait.as_millis()),
                ));
            }
            Err(TryLockError::Error(error)) => return Err(error),
        }
    }
}

/// Why the vocabulary file couldn't be read.
enum ReadError {
    /// It couldn't be opened or read.
    Unreadable(io::Error),
    /// It doesn't parse, such as a file cut short.
    Damaged(serde_json::Error),
}

/// Reads the vocabulary file at `path`: `None` when there is none.
fn read_file(path: &Path) -> Result<Option<SavedVocabulary>, ReadError> {
    let data = match std::fs::read(path) {
        Ok(data) => data,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ReadError::Unreadable(error)),
    };
    serde_json::from_slice(&data)
        .map(Some)
        .map_err(ReadError::Damaged)
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

impl SavedVocabulary {
    /// Drops the n-grams of a file saved before version 2, which an older
    /// tokenizer made, and keeps its words. Returns whether it did.
    fn migrate(&mut self) -> bool {
        let migrated = self.version < MODEL_VERSION;
        if migrated {
            self.bigrams.clear();
            self.trigrams.clear();
        }
        migrated
    }
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

/// `counts` as `vocabulary.json` holds them.
fn to_json(counts: &Counts) -> serde_json::Result<Vec<u8>> {
    serde_json::to_vec(&Saving(counts))
}

/// `counts` as `vocabulary.json` holds them, in at most `cap` bytes. Larger
/// counts first drop their rarest entries, until they fit in seven eighths of
/// `cap`, so they have room to grow before they prune again: the trigrams
/// seen once, then the bigrams and then the words, then those seen twice,
/// and so on.
fn pruned_json(counts: &mut Counts, cap: u64) -> serde_json::Result<Vec<u8>> {
    let mut data = to_json(counts)?;
    if data.len() as u64 <= cap {
        return Ok(data);
    }
    let before = data.len();
    let target = cap / 8 * 7;
    let mut floor = 1;
    'pruning: loop {
        for level in [Level::Trigrams, Level::Bigrams, Level::Words] {
            counts.prune(level, floor);
            data = to_json(counts)?;
            if data.len() as u64 <= target {
                break 'pruning;
            }
        }
        floor += 1;
    }
    log::info!(
        "The word suggestions' vocabulary reached {before} bytes. It dropped the entries seen \
         {floor} times or fewer and is now {} bytes.",
        data.len()
    );
    Ok(data)
}

/// A save's work, which can run off the UI thread: adding what was learned
/// to the file under its lock.
struct SaveJob {
    path: PathBuf,
    cap: u64,
    lock_wait: Duration,
    /// What was learned since the last save.
    learned: Counts,
    /// The whole vocabulary, to write in place of a file gone or damaged. A
    /// background save copies it only once a save has found it needed.
    whole: Option<Counts>,
}

/// What a [`SaveJob`] did.
enum Saved {
    /// It wrote the file. These are its counts.
    Written(Counts),
    /// Nothing: the file is gone or damaged, and the job had no whole
    /// vocabulary to write in its place.
    NeedsWhole,
}

impl SaveJob {
    /// Takes the lock, reads the file, adds the learning, prunes to the cap
    /// and writes the file whole: beside the old one first, which it then
    /// replaces, so a crash leaves one or the other whole. On an error the
    /// job keeps its learning and whole vocabulary.
    fn run(&mut self) -> io::Result<Saved> {
        let path = &self.path;
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let lock = match lock(path, self.lock_wait) {
            Ok(lock) => Some(lock),
            Err(error) if error.kind() == io::ErrorKind::TimedOut => return Err(error),
            // A file system that can't lock files saves as if one jot ran.
            Err(error) => {
                log::warn!(
                    "Couldn't lock the word suggestions' vocabulary {} ({error}). It is saved \
                     without the lock.",
                    path.display()
                );
                None
            }
        };
        let mut merged = match read_file(path) {
            Ok(Some(mut saved)) => {
                saved.migrate();
                let mut counts = Counts::from_saved(saved);
                counts.add(&self.learned);
                Some(counts)
            }
            // Gone or damaged, the file has nothing to add to this jot's
            // vocabulary, which takes its place.
            Ok(None) if self.whole.is_some() => None,
            Err(ReadError::Damaged(error)) if self.whole.is_some() => {
                match set_aside(path) {
                    Ok(kept) => log::warn!(
                        "The word suggestions' vocabulary {} is damaged ({error}). It was kept as \
                         {}, and this session's vocabulary took its place.",
                        path.display(),
                        kept.display()
                    ),
                    Err(move_error) if move_error.kind() == io::ErrorKind::NotFound => {}
                    Err(move_error) => {
                        return Err(io::Error::other(format!(
                            "it is damaged ({error}), and couldn't be moved aside ({move_error})"
                        )));
                    }
                }
                None
            }
            Ok(None) | Err(ReadError::Damaged(_)) => return Ok(Saved::NeedsWhole),
            Err(ReadError::Unreadable(error)) => return Err(error),
        };
        let counts = match &mut merged {
            Some(merged) => merged,
            None => self.whole.as_mut().expect("a whole vocabulary"),
        };
        let data = pruned_json(counts, self.cap)?;
        write_whole(path, &data)?;
        drop(lock);
        Ok(Saved::Written(match merged {
            Some(merged) => merged,
            None => self.whole.take().expect("a whole vocabulary"),
        }))
    }
}

/// A save begun on the UI thread, to run on another.
struct BackgroundSave {
    job: SaveJob,
    result: mpsc::Sender<io::Result<Saved>>,
}

impl BackgroundSave {
    fn run(mut self) {
        let _ = self.result.send(self.job.run());
    }
}

/// A save running in the background.
struct InFlight {
    /// The learning it took, which goes back to the unsaved learning if the
    /// save fails.
    learned: Counts,
    /// Where its result arrives.
    result: mpsc::Receiver<io::Result<Saved>>,
}

/// Frees `counts` on `executor`, off the UI thread: freeing a vocabulary at
/// its size cap takes a fifth of a second.
fn drop_in_background(counts: Option<Counts>, executor: &BackgroundExecutor) {
    if let Some(counts) = counts {
        executor.spawn(async move { drop(counts) }).detach();
    }
}

/// Logs a save that failed. Its learning stays for the next save.
fn log_failed_save(path: &Path, error: &io::Error) {
    log::error!(
        "Couldn't save the word suggestions' vocabulary {}: {error}. What was learned since \
         the last save is kept for the next one.",
        path.display()
    );
}

/// The persistent, cross-document vocabulary learned from typed text.
pub struct SharedVocabulary {
    counts: Counts,
    /// What it learned since it last saved, or since a background save
    /// began, which the next save adds to the file.
    unsaved: Counts,
    dictionary: Rc<Dictionary>,
    /// Whether there is anything to save: learning, or a file from an
    /// earlier version to write again.
    dirty: bool,
    /// The file it saves to: none in tests, or when the file there couldn't
    /// be read and mustn't be written over.
    file: Option<PathBuf>,
    /// How long a save waits for another jot to finish saving.
    lock_wait: Duration,
    /// The background save under way.
    in_flight: Option<InFlight>,
    /// Whether another save was asked for while one ran.
    save_again: bool,
    /// Whether the last background save found the file gone or damaged, so
    /// the next one sends the whole vocabulary.
    needs_whole: bool,
    /// How many background saves began.
    #[cfg(test)]
    background_saves: usize,
}

impl SharedVocabulary {
    /// Creates an empty vocabulary with no loaded dictionary.
    pub fn new() -> Self {
        Self {
            counts: Counts::default(),
            unsaved: Counts::default(),
            dictionary: Rc::new(Dictionary::empty()),
            dirty: false,
            file: None,
            lock_wait: LOCK_WAIT,
            in_flight: None,
            save_again: false,
            needs_whole: false,
            #[cfg(test)]
            background_saves: 0,
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
        let dictionary = Rc::new(Dictionary::load_default());
        match dirs::config_dir() {
            Some(config) => Self::load_from(config.join("jot").join("vocabulary.json"), dictionary),
            None => Self::with_dictionary(dictionary),
        }
    }

    /// Loads the vocabulary saved at `path`, to save there from then on.
    ///
    /// A file that doesn't parse, such as one cut short, is never written
    /// over: it is moved aside to a name beside it, and learning starts
    /// afresh. One that can't be read or moved is left alone, and nothing is
    /// saved this session.
    fn load_from(path: PathBuf, dictionary: Rc<Dictionary>) -> Self {
        let mut vocabulary = Self::with_dictionary(dictionary);
        match read_file(&path) {
            Ok(saved) => {
                if let Some(saved) = saved {
                    vocabulary.read(saved);
                }
                vocabulary.file = Some(path);
            }
            Err(ReadError::Unreadable(error)) => log::error!(
                "Couldn't read the word suggestions' vocabulary {}: {error}. It won't be saved \
                 over this session.",
                path.display()
            ),
            Err(ReadError::Damaged(error)) => match set_aside(&path) {
                Ok(kept) => {
                    log::warn!(
                        "The word suggestions' vocabulary {} is damaged ({error}). It was kept as {}, \
                         and a new one started.",
                        path.display(),
                        kept.display()
                    );
                    vocabulary.file = Some(path);
                }
                // Another jot starting at the same time moved it first.
                Err(move_error) if move_error.kind() == io::ErrorKind::NotFound => {
                    vocabulary.file = Some(path);
                }
                Err(move_error) => log::error!(
                    "The word suggestions' vocabulary {} is damaged ({error}), and couldn't be moved \
                     aside ({move_error}). It won't be saved over this session.",
                    path.display()
                ),
            },
        }
        vocabulary
    }

    /// Takes the counts of a file. One saved before version 2 keeps its
    /// words and drops its n-grams.
    fn read(&mut self, mut saved: SavedVocabulary) {
        let migrated = saved.migrate();
        self.counts = Counts::from_saved(saved);
        self.dirty = migrated;
    }

    /// The vocabulary as `vocabulary.json` holds it.
    #[cfg(test)]
    fn to_json(&self) -> serde_json::Result<Vec<u8>> {
        to_json(&self.counts)
    }

    /// Returns the spelling dictionary shared with autocomplete hygiene.
    pub fn dictionary(&self) -> Rc<Dictionary> {
        self.dictionary.clone()
    }

    /// Adds what it learned since its last save to the file, if anything,
    /// on this thread. A background save under way is waited for first, so
    /// its learning counts once. For quitting, and for closing the last
    /// window, where the save has to finish.
    pub fn save(&mut self) {
        self.save_within(VOCAB_MAX_BYTES);
    }

    /// Saves in at most `cap` bytes, pruning to fit. What a save couldn't
    /// write stays for the next one.
    fn save_within(&mut self, cap: u64) {
        self.finish_background_save(true);
        if !self.dirty {
            return;
        }
        let Some(path) = self.file.clone() else {
            return;
        };
        let mut job = SaveJob {
            path,
            cap,
            lock_wait: self.lock_wait,
            learned: std::mem::take(&mut self.unsaved),
            whole: Some(std::mem::take(&mut self.counts)),
        };
        match job.run() {
            Ok(Saved::Written(counts)) => {
                self.counts = counts;
                self.dirty = false;
                self.needs_whole = false;
            }
            result => {
                if let Err(error) = result {
                    log_failed_save(&job.path, &error);
                }
                self.unsaved = job.learned;
                self.counts = job.whole.take().unwrap_or_default();
            }
        }
    }

    /// Saves as [`save`](Self::save) does, with the file's work on a
    /// background thread, so the UI never waits for it. The task finishes
    /// when the save does.
    ///
    /// The save takes what was learned up to now. Learning while it runs
    /// stays unsaved for the next save, and once it is done the vocabulary is
    /// the file as written plus that learning. A save asked for while one
    /// runs follows it, once however often it was asked for. A save that
    /// fails puts its learning back, so the next save adds it.
    ///
    /// Detach the task, or keep it. Dropping it is safe too: a save already
    /// running finishes, and the next save takes in its result.
    pub fn save_in_background(vocabulary: &Rc<RefCell<Self>>, cx: &App) -> Task<()> {
        let Some(running) = Self::start_background_save(vocabulary, cx.background_executor())
        else {
            return Task::ready(());
        };
        let vocabulary = vocabulary.clone();
        cx.spawn(async move |cx| {
            let mut running = running;
            loop {
                running.await;
                let (replaced, again) = {
                    let mut this = vocabulary.borrow_mut();
                    let replaced = this.finish_background_save(false);
                    (replaced, std::mem::take(&mut this.save_again))
                };
                drop_in_background(replaced, cx.background_executor());
                if !again {
                    return;
                }
                match Self::start_background_save(&vocabulary, cx.background_executor()) {
                    Some(next) => running = next,
                    None => return,
                }
            }
        })
    }

    /// Starts a save of the unsaved learning on `executor`, once a save
    /// under way has finished.
    fn start_background_save(
        vocabulary: &Rc<RefCell<Self>>,
        executor: &BackgroundExecutor,
    ) -> Option<Task<()>> {
        let (replaced, save) = {
            let mut this = vocabulary.borrow_mut();
            let replaced = this.finish_background_save(false);
            (replaced, this.begin_background_save())
        };
        drop_in_background(replaced, executor);
        let save = save?;
        Some(executor.spawn(async move { save.run() }))
    }

    /// Takes the unsaved learning for a save on another thread. With a save
    /// under way, or nothing to save, there is none, and a save under way
    /// notes that another is wanted.
    fn begin_background_save(&mut self) -> Option<BackgroundSave> {
        if self.in_flight.is_some() {
            self.save_again = true;
            return None;
        }
        if !self.dirty {
            return None;
        }
        let path = self.file.clone()?;
        let learned = std::mem::take(&mut self.unsaved);
        let job = SaveJob {
            path,
            cap: VOCAB_MAX_BYTES,
            lock_wait: self.lock_wait,
            learned: learned.clone(),
            whole: self.needs_whole.then(|| self.counts.clone()),
        };
        let (sender, result) = mpsc::channel();
        self.in_flight = Some(InFlight { learned, result });
        self.dirty = false;
        #[cfg(test)]
        {
            self.background_saves += 1;
        }
        Some(BackgroundSave {
            job,
            result: sender,
        })
    }

    /// Takes in the result of the background save under way, when it has
    /// one, waiting for it if `wait`. A save written becomes the vocabulary,
    /// with the learning since it began on top, and the vocabulary it
    /// replaces is returned, since freeing it takes a while. One that failed,
    /// or never ran, puts its learning back.
    fn finish_background_save(&mut self, wait: bool) -> Option<Counts> {
        let in_flight = self.in_flight.take()?;
        let result = if wait {
            in_flight.result.recv().ok()
        } else {
            match in_flight.result.try_recv() {
                Ok(result) => Some(result),
                Err(mpsc::TryRecvError::Empty) => {
                    self.in_flight = Some(in_flight);
                    return None;
                }
                Err(mpsc::TryRecvError::Disconnected) => None,
            }
        };
        match result {
            Some(Ok(Saved::Written(mut counts))) => {
                counts.add(&self.unsaved);
                self.needs_whole = false;
                Some(std::mem::replace(&mut self.counts, counts))
            }
            result => {
                match result {
                    Some(Ok(Saved::NeedsWhole)) => {
                        self.needs_whole = true;
                        self.save_again = true;
                    }
                    Some(Err(error)) => {
                        if let Some(path) = &self.file {
                            log_failed_save(path, &error);
                        }
                    }
                    _ => {}
                }
                let mut learned = in_flight.learned;
                learned.add(&self.unsaved);
                self.unsaved = learned;
                self.dirty = true;
                None
            }
        }
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
            self.unsaved.add_run(starts_sentence, &run);
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
        self.unsaved.add_word(word, 1);
        self.dirty = true;
    }

    /// Counts `second` as having followed `first` once more.
    #[cfg(test)]
    pub(super) fn learn_bigram(&mut self, first: &str, second: &str) {
        for counts in [&mut self.counts, &mut self.unsaved] {
            counts
                .bigrams
                .entry(fold(first).as_ref().into())
                .or_default()
                .add(second, &fold(second), 1);
        }
        self.dirty = true;
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
    use gpui_kit::TestAppContext;

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

    /// A folder for one test's files, removed afterwards.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(name: &str) -> Self {
            let folder =
                std::env::temp_dir().join(format!("jot-vocabulary-{}-{name}", std::process::id()));
            let _ = std::fs::remove_dir_all(&folder);
            std::fs::create_dir_all(&folder).unwrap();
            Self(folder)
        }

        fn file(&self) -> PathBuf {
            self.0.join("vocabulary.json")
        }

        fn names(&self) -> Vec<String> {
            let mut names: Vec<String> = std::fs::read_dir(&self.0)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }

        fn load(&self) -> SharedVocabulary {
            SharedVocabulary::load_from(self.file(), Rc::new(Dictionary::empty()))
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn saving_replaces_the_file_whole() {
        let scratch = Scratch::new("save");
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Ferns grow slowly");
        vocabulary.save();
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        assert_eq!(scratch.names(), ["vocabulary.json", "vocabulary.lock"]);
        let reloaded = scratch.load();
        assert_eq!(reloaded.counts.word_count("grow"), 2);
        assert!(!reloaded.dirty);
    }

    /// A damaged file, such as one a crash cut short, isn't written over: it
    /// is kept beside the new one.
    #[test]
    fn a_damaged_file_is_kept() {
        let scratch = Scratch::new("damaged");
        let damaged = br#"{"version":2,"words":{"Ferns":3,"gr"#;
        std::fs::write(scratch.file(), damaged).unwrap();
        let mut vocabulary = scratch.load();
        assert_eq!(vocabulary.counts.total(), 0);
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        assert_eq!(
            scratch.names(),
            [
                "vocabulary.damaged.json",
                "vocabulary.json",
                "vocabulary.lock"
            ]
        );
        assert_eq!(
            std::fs::read(scratch.0.join("vocabulary.damaged.json")).unwrap(),
            damaged
        );
        assert_eq!(scratch.load().counts.word_count("mosses"), 1);

        std::fs::write(scratch.file(), "not a vocabulary").unwrap();
        scratch.load();
        assert_eq!(
            scratch.names(),
            [
                "vocabulary.damaged-2.json",
                "vocabulary.damaged.json",
                "vocabulary.lock"
            ]
        );
    }

    /// One that can't be read at all is left alone, and not saved over.
    #[test]
    fn an_unreadable_file_is_left_alone() {
        let scratch = Scratch::new("unreadable");
        std::fs::create_dir(scratch.file()).unwrap();
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        assert!(scratch.file().is_dir());
        assert_eq!(scratch.names(), ["vocabulary.json"]);
    }

    /// Past its cap, a vocabulary drops its rarest entries, trigrams first,
    /// and goes on learning.
    #[test]
    fn a_vocabulary_past_its_cap_drops_its_rarest_entries() {
        let scratch = Scratch::new("cap");
        let mut vocabulary = scratch.load();
        for _ in 0..5 {
            vocabulary.learn_sentence("Thanks for the update");
        }
        for number in 0..200 {
            vocabulary.learn_sentence(&format!("Notes on topic{number} today"));
        }
        let cap = vocabulary.to_json().unwrap().len() as u64 / 2;
        vocabulary.save_within(cap);
        let saved = std::fs::metadata(scratch.file()).unwrap().len();
        assert!(saved <= cap / 8 * 7, "{saved} bytes, cap {cap}");

        let reloaded = scratch.load();
        assert_eq!(trigram(&reloaded, "thanks", "for", "the"), 5);
        assert_eq!(bigram(&reloaded, "notes", "on"), 200);
        assert_eq!(trigram(&reloaded, "notes", "on", "topic7"), 0);
        assert_eq!(bigram(&reloaded, "on", "topic7"), 0);
        assert_eq!(reloaded.counts.word_count("topic7"), 1);

        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save_within(cap);
        assert_eq!(scratch.load().counts.word_count("mosses"), 1);
    }

    /// The file `scratch` holds.
    fn saved(scratch: &Scratch) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(scratch.file()).unwrap()).unwrap()
    }

    /// The file one vocabulary would save after learning `sentences`.
    fn learned_from<'a>(sentences: impl IntoIterator<Item = &'a str>) -> serde_json::Value {
        let mut vocabulary = SharedVocabulary::new();
        for sentence in sentences {
            vocabulary.learn_sentence(sentence);
        }
        serde_json::from_slice(&vocabulary.to_json().unwrap()).unwrap()
    }

    /// Two jots on one file each add what they learned to what the file
    /// holds, so every count is the sum, whichever saves first.
    #[test]
    fn jots_on_one_file_add_up_their_learning() {
        let first_learns = [
            "Ferns grow slowly",
            "Mosses grow faster",
            "Ferns grow slowly",
        ];
        let second_learns = [
            "Ferns grow slowly",
            "ferns need shade",
            "Lichens grow nowhere",
        ];
        for first_saves_first in [true, false] {
            let scratch = Scratch::new(&format!("merge-{first_saves_first}"));
            let mut earlier = scratch.load();
            earlier.learn_sentence("Ferns grow slowly");
            earlier.save();

            let mut first = scratch.load();
            let mut second = scratch.load();
            for sentence in first_learns {
                first.learn_sentence(sentence);
            }
            for sentence in second_learns {
                second.learn_sentence(sentence);
            }
            let (one, other) = if first_saves_first {
                (&mut first, &mut second)
            } else {
                (&mut second, &mut first)
            };
            one.save();
            other.save();
            let mut all = vec!["Ferns grow slowly"];
            all.extend(first_learns);
            all.extend(second_learns);
            assert_eq!(saved(&scratch), learned_from(all.iter().copied()));
            // The one that saved last works from the sum.
            let worked_from: serde_json::Value =
                serde_json::from_slice(&other.to_json().unwrap()).unwrap();
            assert_eq!(worked_from, learned_from(all.iter().copied()));

            // A save adds only what was learned since the last one.
            one.learn_sentence("Mosses grow faster");
            one.save();
            other.save();
            all.push("Mosses grow faster");
            assert_eq!(saved(&scratch), learned_from(all.iter().copied()));
        }
    }

    /// A save that can't get the lock in time keeps its learning, and a
    /// later one adds it.
    #[test]
    fn a_save_kept_from_the_lock_keeps_its_learning() {
        let scratch = Scratch::new("locked");
        let mut vocabulary = scratch.load();
        vocabulary.lock_wait = Duration::from_millis(100);
        vocabulary.learn_sentence("Ferns grow slowly");
        let held = lock(&scratch.file(), Duration::ZERO).unwrap();
        let started = Instant::now();
        vocabulary.save();
        assert!(started.elapsed() >= Duration::from_millis(100));
        assert!(vocabulary.dirty);
        assert!(!scratch.file().exists());

        drop(held);
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        assert!(!vocabulary.dirty);
        assert_eq!(
            saved(&scratch),
            learned_from(["Ferns grow slowly", "Mosses grow faster"])
        );
    }

    /// A jot that ends without saving, as in a crash, loses only what it
    /// learned since its last save.
    #[test]
    fn a_crash_loses_only_what_that_jot_learned_since_its_last_save() {
        let scratch = Scratch::new("crash");
        let mut first = scratch.load();
        let mut second = scratch.load();
        first.learn_sentence("Ferns grow slowly");
        first.save();
        second.learn_sentence("Mosses grow faster");
        second.save();
        first.learn_sentence("Lichens grow nowhere");
        drop(first);
        second.learn_sentence("Ferns need shade");
        second.save();
        assert_eq!(
            saved(&scratch),
            learned_from([
                "Ferns grow slowly",
                "Mosses grow faster",
                "Ferns need shade"
            ])
        );
    }

    /// A file damaged by the time of a save is kept aside, as at loading,
    /// and one gone is written again. Either way this jot's vocabulary takes
    /// its place.
    #[test]
    fn a_file_damaged_or_gone_by_a_save_gives_way_to_this_vocabulary() {
        let scratch = Scratch::new("damaged-at-save");
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Ferns grow slowly");
        vocabulary.save();
        vocabulary.learn_sentence("Mosses grow faster");
        std::fs::write(scratch.file(), "not a vocabulary").unwrap();
        vocabulary.save();
        assert_eq!(
            scratch.names(),
            [
                "vocabulary.damaged.json",
                "vocabulary.json",
                "vocabulary.lock"
            ]
        );
        assert_eq!(
            std::fs::read(scratch.0.join("vocabulary.damaged.json")).unwrap(),
            b"not a vocabulary"
        );
        assert_eq!(
            saved(&scratch),
            learned_from(["Ferns grow slowly", "Mosses grow faster"])
        );

        std::fs::remove_file(scratch.file()).unwrap();
        vocabulary.learn_sentence("Lichens grow nowhere");
        vocabulary.save();
        assert_eq!(
            saved(&scratch),
            learned_from([
                "Ferns grow slowly",
                "Mosses grow faster",
                "Lichens grow nowhere"
            ])
        );
    }

    /// A file an earlier version wrote meanwhile keeps its words and drops
    /// its n-grams, as at loading.
    #[test]
    fn a_file_from_an_earlier_version_is_migrated_as_it_merges() {
        let scratch = Scratch::new("migrate-at-save");
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Ferns grow slowly");
        std::fs::write(
            scratch.file(),
            r#"{"words":{"Ferns":4},"bigrams":{"Ferns":{"grow":4}}}"#,
        )
        .unwrap();
        vocabulary.save();
        let reloaded = scratch.load();
        assert_eq!(reloaded.counts.word_count("ferns"), 5);
        assert_eq!(bigram(&reloaded, "ferns", "grow"), 1);
        assert!(!reloaded.dirty);
    }

    /// An entry one jot pruned comes back when another saves after seeing
    /// it, with only what that one saw since its last save.
    #[test]
    fn a_pruned_entry_comes_back_with_the_new_count() {
        let scratch = Scratch::new("prune-and-merge");
        let mut first = scratch.load();
        let mut second = scratch.load();
        for _ in 0..5 {
            first.learn_sentence("Thanks for the update");
        }
        for number in 0..200 {
            first.learn_sentence(&format!("Notes on topic{number} today"));
        }
        let cap = first.to_json().unwrap().len() as u64 / 2;
        first.save_within(cap);
        assert_eq!(bigram(&scratch.load(), "on", "topic7"), 0);

        second.learn_sentence("Notes on topic7 today");
        second.save();
        let reloaded = scratch.load();
        assert_eq!(bigram(&reloaded, "on", "topic7"), 1);
        assert_eq!(reloaded.counts.word_count("topic7"), 2);
        assert_eq!(bigram(&reloaded, "notes", "on"), 201);
    }

    /// A vocabulary on `scratch`'s file, shared as jot shares it.
    fn shared(scratch: &Scratch) -> Rc<RefCell<SharedVocabulary>> {
        Rc::new(RefCell::new(scratch.load()))
    }

    /// Starts a background save, as saving a file in jot does.
    fn save_in_background(vocabulary: &Rc<RefCell<SharedVocabulary>>, cx: &mut TestAppContext) {
        cx.update(|cx| SharedVocabulary::save_in_background(vocabulary, cx).detach());
    }

    /// What `vocabulary` holds, as it would save it whole.
    fn held(vocabulary: &Rc<RefCell<SharedVocabulary>>) -> serde_json::Value {
        serde_json::from_slice(&vocabulary.borrow().to_json().unwrap()).unwrap()
    }

    /// Learning while a background save runs is saved once, by the next
    /// save. Meanwhile the vocabulary is the file as written, another jot's
    /// learning included, plus that learning.
    #[gpui_kit::test]
    fn learning_during_a_background_save_counts_once(cx: &mut TestAppContext) {
        let scratch = Scratch::new("background");
        let vocabulary = shared(&scratch);
        vocabulary.borrow_mut().learn_sentence("Ferns grow slowly");
        save_in_background(&vocabulary, cx);
        vocabulary.borrow_mut().learn_sentence("Mosses grow faster");
        let mut other = scratch.load();
        other.learn_sentence("Lichens grow nowhere");
        other.save();
        cx.run_until_parked();

        assert_eq!(
            saved(&scratch),
            learned_from(["Lichens grow nowhere", "Ferns grow slowly"])
        );
        let all = [
            "Lichens grow nowhere",
            "Ferns grow slowly",
            "Mosses grow faster",
        ];
        assert_eq!(held(&vocabulary), learned_from(all));
        assert!(vocabulary.borrow().dirty);

        save_in_background(&vocabulary, cx);
        cx.run_until_parked();
        assert_eq!(saved(&scratch), learned_from(all));
        assert_eq!(held(&vocabulary), learned_from(all));
        assert!(!vocabulary.borrow().dirty);
    }

    /// A background save that fails puts its learning back, and the next
    /// save adds it.
    #[gpui_kit::test]
    fn a_failed_background_save_keeps_its_learning(cx: &mut TestAppContext) {
        let scratch = Scratch::new("background-failed");
        let vocabulary = shared(&scratch);
        vocabulary.borrow_mut().lock_wait = Duration::from_millis(50);
        vocabulary.borrow_mut().learn_sentence("Ferns grow slowly");
        let held_lock = lock(&scratch.file(), Duration::ZERO).unwrap();
        save_in_background(&vocabulary, cx);
        vocabulary.borrow_mut().learn_sentence("Mosses grow faster");
        cx.run_until_parked();
        assert!(!scratch.file().exists());
        assert!(vocabulary.borrow().dirty);
        assert!(vocabulary.borrow().in_flight.is_none());

        drop(held_lock);
        save_in_background(&vocabulary, cx);
        cx.run_until_parked();
        let both = ["Ferns grow slowly", "Mosses grow faster"];
        assert_eq!(saved(&scratch), learned_from(both));
        assert_eq!(held(&vocabulary), learned_from(both));
    }

    /// Saves asked for while one runs make one more save after it.
    #[gpui_kit::test]
    fn saves_asked_for_during_a_save_make_one_more(cx: &mut TestAppContext) {
        let scratch = Scratch::new("background-again");
        let vocabulary = shared(&scratch);
        vocabulary.borrow_mut().learn_sentence("Ferns grow slowly");
        save_in_background(&vocabulary, cx);
        vocabulary.borrow_mut().learn_sentence("Mosses grow faster");
        save_in_background(&vocabulary, cx);
        vocabulary
            .borrow_mut()
            .learn_sentence("Lichens grow nowhere");
        save_in_background(&vocabulary, cx);
        cx.run_until_parked();

        assert_eq!(vocabulary.borrow().background_saves, 2);
        let all = [
            "Ferns grow slowly",
            "Mosses grow faster",
            "Lichens grow nowhere",
        ];
        assert_eq!(saved(&scratch), learned_from(all));
        assert!(!vocabulary.borrow().dirty);
    }

    /// A background save that finds the file gone asks for the whole
    /// vocabulary, and the save after it writes it.
    #[gpui_kit::test]
    fn a_background_save_writes_the_whole_vocabulary_over_a_gone_file(cx: &mut TestAppContext) {
        let scratch = Scratch::new("background-gone");
        let vocabulary = shared(&scratch);
        vocabulary.borrow_mut().learn_sentence("Ferns grow slowly");
        vocabulary.borrow_mut().save();
        std::fs::remove_file(scratch.file()).unwrap();
        vocabulary.borrow_mut().learn_sentence("Mosses grow faster");
        save_in_background(&vocabulary, cx);
        cx.run_until_parked();

        assert_eq!(vocabulary.borrow().background_saves, 2);
        let both = ["Ferns grow slowly", "Mosses grow faster"];
        assert_eq!(saved(&scratch), learned_from(both));
        assert_eq!(held(&vocabulary), learned_from(both));
        assert!(!vocabulary.borrow().needs_whole);
    }

    /// A save on the UI thread, as at quitting, waits for a background save
    /// under way, so its learning counts once.
    #[test]
    fn a_save_at_quit_waits_for_a_background_save() {
        let scratch = Scratch::new("quit-during-save");
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Ferns grow slowly");
        let save = vocabulary.begin_background_save().unwrap();
        let running = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(50));
            save.run();
        });
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        running.join().unwrap();
        let both = ["Ferns grow slowly", "Mosses grow faster"];
        assert_eq!(saved(&scratch), learned_from(both));
        assert!(!vocabulary.dirty);
    }

    /// A background save that never ran, its task dropped before it started,
    /// puts its learning back.
    #[test]
    fn a_background_save_that_never_ran_keeps_its_learning() {
        let scratch = Scratch::new("never-ran");
        let mut vocabulary = scratch.load();
        vocabulary.learn_sentence("Ferns grow slowly");
        drop(vocabulary.begin_background_save().unwrap());
        vocabulary.learn_sentence("Mosses grow faster");
        vocabulary.save();
        let both = ["Ferns grow slowly", "Mosses grow faster"];
        assert_eq!(saved(&scratch), learned_from(both));
    }

    /// The environment variable that makes [`jot_process`] run: the file,
    /// then on the next line what to do.
    const PROCESS_TASK: &str = "JOT_VOCABULARY_TEST_PROCESS";

    /// What [`jot_process`] prints once it holds the lock.
    const LOCKED: &str = "the other jot holds the lock";

    /// Another jot, running [`jot_process`] in a process of its own. It is
    /// killed if the test ends first.
    struct OtherJot(Option<std::process::Child>);

    impl OtherJot {
        fn spawn(file: &Path, task: &str) -> Self {
            let tests = module_path!().split_once("::").unwrap().1;
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([&format!("{tests}::jot_process"), "--exact", "--ignored"])
                .args(["--nocapture", "--test-threads=1"])
                .env(PROCESS_TASK, format!("{}\n{task}", file.display()))
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            Self(Some(child))
        }

        /// Waits for it to finish, and checks that it succeeded.
        fn finish(mut self) {
            let output = self.0.take().unwrap().wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    impl Drop for OtherJot {
        fn drop(&mut self) {
            if let Some(child) = &mut self.0 {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    /// Another jot, for the tests that run several processes. `learn N
    /// sentence` learns the sentence and saves, N times. `crash` learns and
    /// saves a sentence, learns another, then holds the lock until it is
    /// killed.
    #[test]
    #[ignore = "run as a process of its own by the tests with several jots"]
    fn jot_process() {
        let Ok(task) = std::env::var(PROCESS_TASK) else {
            return;
        };
        let (file, task) = task.split_once('\n').unwrap();
        let file = PathBuf::from(file);
        let mut vocabulary =
            SharedVocabulary::load_from(file.clone(), Rc::new(Dictionary::empty()));
        if let Some(task) = task.strip_prefix("learn ") {
            let (times, sentence) = task.split_once(' ').unwrap();
            for _ in 0..times.parse().unwrap() {
                vocabulary.learn_sentence(sentence);
                vocabulary.save();
                assert!(!vocabulary.dirty, "a save failed");
            }
        } else {
            assert_eq!(task, "crash");
            vocabulary.learn_sentence("Ferns grow slowly");
            vocabulary.save();
            vocabulary.learn_sentence("Mosses grow faster");
            let _held = lock(&file, Duration::ZERO).unwrap();
            println!("{LOCKED}");
            io::stdout().flush().unwrap();
            io::stdin().read_line(&mut String::new()).unwrap();
        }
    }

    /// jots in processes of their own, saving at the same time as each
    /// other, add up their learning.
    #[test]
    fn jots_in_separate_processes_add_up_their_learning() {
        let scratch = Scratch::new("processes");
        let jots = ["Ferns grow slowly", "Mosses grow faster"]
            .map(|sentence| OtherJot::spawn(&scratch.file(), &format!("learn 50 {sentence}")));
        let mut vocabulary = scratch.load();
        for _ in 0..50 {
            vocabulary.learn_sentence("Ferns need shade");
            vocabulary.save();
            assert!(!vocabulary.dirty);
        }
        for jot in jots {
            jot.finish();
        }
        let all = [
            "Ferns grow slowly",
            "Mosses grow faster",
            "Ferns need shade",
        ];
        assert_eq!(saved(&scratch), learned_from(all.repeat(50)));
    }

    /// A jot killed while it holds the lock leaves it free, and loses only
    /// what it learned since its last save.
    #[test]
    fn a_jot_killed_holding_the_lock_leaves_it_free() {
        let scratch = Scratch::new("killed");
        let mut jot = OtherJot::spawn(&scratch.file(), "crash");
        let output = jot.0.as_mut().unwrap().stdout.take().unwrap();
        let (locked, has_locked) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in io::BufRead::lines(io::BufReader::new(output)) {
                if line.is_ok_and(|line| line.contains(LOCKED)) {
                    let _ = locked.send(());
                }
            }
        });
        has_locked
            .recv_timeout(Duration::from_secs(60))
            .expect("the other jot took the lock");

        let mut vocabulary = scratch.load();
        vocabulary.lock_wait = Duration::from_millis(100);
        vocabulary.learn_sentence("Lichens grow nowhere");
        vocabulary.save();
        assert!(vocabulary.dirty, "the other jot holds the lock");

        drop(jot);
        vocabulary.save();
        assert!(!vocabulary.dirty);
        assert_eq!(
            saved(&scratch),
            learned_from(["Ferns grow slowly", "Lichens grow nowhere"])
        );
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
